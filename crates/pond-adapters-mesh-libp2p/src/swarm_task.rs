//! The background task that owns the `Swarm` exclusively, driven by a command
//! channel and emitting inbound frames to an event channel. `Swarm` is not
//! shareable behind `Arc<dyn MeshTransport>`, so this is the standard libp2p
//! integration pattern; see the `pond_adapters_mesh_libp2p` crate-level notes.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use libp2p::core::multiaddr::Protocol;
use libp2p::request_response::{self, OutboundRequestId};
use libp2p::swarm::dial_opts::{DialOpts, PeerCondition};
use libp2p::swarm::SwarmEvent;
use libp2p::{gossipsub, identify, kad, noise, relay, tcp, yamux, Multiaddr, Swarm};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use pond_core::mesh::domain::hashes::{HarnessHash, ModelHash};
use pond_core::mesh::domain::peer_id::PeerId as DomainPeerId;
use pond_core::mesh::ports::mesh_transport::MeshTransportError;
use pond_core::mesh::ports::peer_directory::PeerDirectory;
use pond_mesh_protocol::identity::MeshKeypair;
use pond_mesh_protocol::wire::Handshake;

use crate::adapter::Libp2pMeshTransportConfig;
use crate::behaviour::{
    MeshBehaviour, MeshBehaviourEvent, MeshRequest, MeshResponse, MESH_PROTOCOL,
};
use crate::identity::{domain_peer_to_libp2p, to_libp2p_keypair};

type Libp2pPeerId = libp2p::PeerId;
type InboundFrame = (DomainPeerId, Vec<u8>);

pub enum Command {
    Connect {
        peer: DomainPeerId,
        address: String,
        reply: oneshot::Sender<Result<(), MeshTransportError>>,
    },
    Send {
        peer: DomainPeerId,
        frame: Vec<u8>,
        reply: oneshot::Sender<Result<(), MeshTransportError>>,
    },
    ConnectedPeers {
        reply: oneshot::Sender<Vec<DomainPeerId>>,
    },
    /// Test/setup helper — not part of the `MeshTransport` port. Multiaddrs
    /// are libp2p-specific, so this stays adapter-only.
    ListenAddrs {
        reply: oneshot::Sender<Vec<Multiaddr>>,
    },
    /// Reserve a relay slot on an already-reachable peer so this node can be
    /// dialed via `.../p2p-circuit/p2p/<self>` even when it can't accept
    /// direct inbound connections. Adapter-only, same reasoning as above.
    ReserveRelay {
        relay_peer: DomainPeerId,
        relay_address: String,
        reply: oneshot::Sender<Result<(), MeshTransportError>>,
    },
}

pub struct SwarmHandles {
    pub command_tx: mpsc::UnboundedSender<Command>,
    pub inbound_rx: mpsc::UnboundedReceiver<InboundFrame>,
    pub task: JoinHandle<()>,
}

pub async fn spawn(config: Libp2pMeshTransportConfig) -> anyhow::Result<SwarmHandles> {
    let local_handshake_bytes =
        build_local_handshake(&config.keypair, config.harness_hash, config.model_hash)
            .encode_to_vec();
    let swarm = build_swarm(config.keypair, config.listen_addr).await?;

    let (command_tx, command_rx) = mpsc::unbounded_channel();
    let (inbound_tx, inbound_rx) = mpsc::unbounded_channel();

    let event_loop = EventLoop {
        swarm,
        command_rx,
        inbound_tx,
        harness_hash: config.harness_hash,
        model_hash: config.model_hash,
        peer_directory: config.peer_directory,
        local_handshake_bytes,
        pending_connect: HashMap::new(),
        pending_handshake: HashMap::new(),
        connected: HashMap::new(),
        known_addresses: HashMap::new(),
        listen_addrs: Vec::new(),
    };
    let task = tokio::spawn(event_loop.run());

    Ok(SwarmHandles {
        command_tx,
        inbound_rx,
        task,
    })
}

fn build_local_handshake(
    keypair: &MeshKeypair,
    harness: HarnessHash,
    model: ModelHash,
) -> Handshake {
    let unsigned = Handshake::new(keypair.peer_id(), harness, model, [0u8; 64]);
    let signature = keypair.sign(&unsigned.signed_payload());
    Handshake::new(keypair.peer_id(), harness, model, signature)
}

/// The port a WebSocket listener binds to, alongside the plain-TCP one. Free
/// HTTP/TLS tunnels only proxy HTTP/WS, so raw TCP cannot cross them. Offset from
/// the TCP port to keep the "stable per identity" property; `0` stays `0`
/// (OS-assigned, what the tests need) since `0 + 1 = 1` is a privileged port.
fn ws_port_for(tcp_port: u16) -> u16 {
    if tcp_port == 0 {
        0
    } else {
        tcp_port + 1
    }
}

async fn build_swarm(
    keypair: MeshKeypair,
    listen_addr: Multiaddr,
) -> anyhow::Result<Swarm<MeshBehaviour>> {
    let libp2p_keypair = to_libp2p_keypair(&keypair);

    let tcp_port = listen_addr
        .iter()
        .find_map(|p| match p {
            Protocol::Tcp(port) => Some(port),
            _ => None,
        })
        .unwrap_or(0);
    let ws_listen_addr: Multiaddr = format!("/ip4/0.0.0.0/tcp/{}/ws", ws_port_for(tcp_port))
        .parse()
        .expect("valid multiaddr literal");

    let mut swarm = libp2p::SwarmBuilder::with_existing_identity(libp2p_keypair)
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_websocket(noise::Config::new, yamux::Config::default)
        .await?
        .with_relay_client(noise::Config::new, yamux::Config::default)?
        .with_behaviour(|local_keypair, relay_client| {
            let peer_id = local_keypair.public().to_peer_id();
            let gossipsub = gossipsub::Behaviour::new(
                gossipsub::MessageAuthenticity::Signed(local_keypair.clone()),
                gossipsub::Config::default(),
            )?;
            Ok(MeshBehaviour {
                identify: identify::Behaviour::new(identify::Config::new(
                    MESH_PROTOCOL.to_string(),
                    local_keypair.public(),
                )),
                kad: kad::Behaviour::new(peer_id, kad::store::MemoryStore::new(peer_id)),
                gossipsub,
                relay: relay::Behaviour::new(peer_id, relay::Config::default()),
                relay_client,
                dcutr: libp2p::dcutr::Behaviour::new(peer_id),
                mesh_rr: request_response::cbor::Behaviour::new(
                    [(
                        libp2p::StreamProtocol::new(MESH_PROTOCOL),
                        request_response::ProtocolSupport::Full,
                    )],
                    request_response::Config::default(),
                ),
                ping: libp2p::ping::Behaviour::new(libp2p::ping::Config::new()),
            })
        })?
        .with_swarm_config(|cfg| {
            // Comfortably above the ping interval, so the ping heartbeat —
            // not a race against this timeout — is what decides whether an
            // idle-but-reachable connection stays open.
            cfg.with_idle_connection_timeout(Duration::from_secs(60))
        })
        .build();

    swarm.behaviour_mut().kad.set_mode(Some(kad::Mode::Server));
    swarm.listen_on(listen_addr)?;
    swarm.listen_on(ws_listen_addr)?;
    Ok(swarm)
}

struct EventLoop {
    swarm: Swarm<MeshBehaviour>,
    command_rx: mpsc::UnboundedReceiver<Command>,
    inbound_tx: mpsc::UnboundedSender<InboundFrame>,
    harness_hash: HarnessHash,
    model_hash: ModelHash,
    /// Who this Pond trusts. Consulted on every handshake, in both
    /// directions — see `is_trusted`.
    peer_directory: Arc<dyn PeerDirectory>,
    /// Precomputed once — our own identity/hashes never change across
    /// connections, so there's no reason to re-sign per dial.
    local_handshake_bytes: Vec<u8>,
    /// Outbound dial in flight: who asked, and how to reply once the
    /// handshake (not just the raw connection) completes.
    pending_connect: HashMap<
        Libp2pPeerId,
        (
            DomainPeerId,
            oneshot::Sender<Result<(), MeshTransportError>>,
        ),
    >,
    /// Our outbound `Handshake` request, so the eventual response can be
    /// matched back to the peer that sent it.
    pending_handshake: HashMap<OutboundRequestId, Libp2pPeerId>,
    /// Peers whose handshake has been verified in either direction.
    connected: HashMap<Libp2pPeerId, DomainPeerId>,
    /// The last address we were given for each peer we've ever been asked to
    /// dial, so the background retry loop (`retry_disconnected_known_peers`)
    /// has something to redial with — it never learns addresses on its own.
    /// In-memory only: a restart forgets these, same as `connected`.
    known_addresses: HashMap<Libp2pPeerId, (DomainPeerId, Multiaddr)>,
    /// Every address we're confirmed listening on, including relay-circuit
    /// addresses once a reservation is accepted.
    listen_addrs: Vec<Multiaddr>,
}

/// How often the background loop checks for trusted-but-disconnected peers
/// and redials them. Not configurable — this is a low-cost background
/// safety net, not a latency-sensitive path.
const RECONNECT_INTERVAL: Duration = Duration::from_secs(30);

impl EventLoop {
    async fn run(mut self) {
        self.load_known_addresses().await;
        let mut reconnect_tick = tokio::time::interval(RECONNECT_INTERVAL);
        // The first tick fires immediately; nothing is disconnected yet at
        // startup, so that tick is a harmless no-op rather than useful work.
        reconnect_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                event = self.swarm.select_next_some() => self.handle_swarm_event(event).await,
                command = self.command_rx.recv() => match command {
                    Some(command) => self.handle_command(command).await,
                    None => return,
                },
                _ = reconnect_tick.tick() => self.retry_disconnected_known_peers().await,
            }
        }
    }

    /// Seeds `known_addresses` from `PeerDirectory` at startup so the reconnect
    /// loop can redial peers trusted before a restart. Otherwise the map is only
    /// populated by a fresh `Command::Connect`, and a restart silently forgets
    /// every peer until the user re-shares an invite.
    async fn load_known_addresses(&mut self) {
        let rows = match self.peer_directory.known_addresses().await {
            Ok(rows) => rows,
            Err(err) => {
                tracing::warn!("mesh: failed to load persisted peer addresses: {err}");
                return;
            }
        };
        for (peer, addr_str) in rows {
            let Ok(libp2p_peer) = domain_peer_to_libp2p(peer) else {
                continue;
            };
            let Ok(addr) = addr_str.parse::<Multiaddr>() else {
                continue;
            };
            self.known_addresses.insert(libp2p_peer, (peer, addr));
        }
    }

    /// Background half of the force-reconnect path. `Command::Connect` covers an
    /// explicit request; without this, a peer dropping out from under an idle
    /// connection leaves the mesh disconnected until something calls `connect()`.
    /// Every trusted peer with a known address and no live dial is retried.
    async fn retry_disconnected_known_peers(&mut self) {
        let candidates: Vec<(Libp2pPeerId, DomainPeerId, Multiaddr)> = self
            .known_addresses
            .iter()
            .filter(|(libp2p_peer, _)| {
                !self.connected.contains_key(libp2p_peer)
                    && !self.pending_connect.contains_key(libp2p_peer)
            })
            .map(|(libp2p_peer, (peer, addr))| (*libp2p_peer, *peer, addr.clone()))
            .collect();

        for (libp2p_peer, peer, addr) in candidates {
            // Trust may have been revoked since the address was learned —
            // don't keep hammering a peer that was removed from the circle.
            if !Self::is_trusted(self.peer_directory.clone(), peer).await {
                continue;
            }
            // Fire-and-forget: nothing awaits this attempt, so the reply goes to
            // a receiver we drop. `dial_peer` still needs a sender to satisfy
            // `pending_connect`'s shape; the outcome surfaces through the normal
            // `ConnectionEstablished`/`OutgoingConnectionError` handling.
            let (reply, _ignored) = oneshot::channel();
            self.dial_peer(libp2p_peer, peer, addr, reply);
        }
    }

    async fn handle_command(&mut self, command: Command) {
        match command {
            Command::Connect {
                peer,
                address,
                reply,
            } => {
                let libp2p_peer = match domain_peer_to_libp2p(peer) {
                    Ok(p) => p,
                    Err(err) => {
                        let _ =
                            reply.send(Err(MeshTransportError::MalformedAddress(err.to_string())));
                        return;
                    }
                };
                let addr: Multiaddr = match address.parse() {
                    Ok(addr) => addr,
                    Err(_) => {
                        let _ = reply.send(Err(MeshTransportError::MalformedAddress(address)));
                        return;
                    }
                };
                self.known_addresses
                    .insert(libp2p_peer, (peer, addr.clone()));
                // Best-effort: the peer is already trusted (routes.rs adds trust
                // before calling `connect()`), so a row exists to update. If the
                // write fails, the retry loop falls back to the in-memory copy
                // above for the rest of this process's life.
                if let Err(err) = self
                    .peer_directory
                    .record_peer_address(peer, addr.to_string())
                    .await
                {
                    tracing::warn!("mesh: failed to persist last-known address for {peer}: {err}");
                }
                self.dial_peer(libp2p_peer, peer, addr, reply);
            }
            Command::Send { peer, frame, reply } => {
                let libp2p_peer = match domain_peer_to_libp2p(peer) {
                    Ok(p) => p,
                    Err(_) => {
                        let _ = reply.send(Err(MeshTransportError::PeerUnreachable(peer)));
                        return;
                    }
                };
                if !self.connected.contains_key(&libp2p_peer) {
                    let _ = reply.send(Err(MeshTransportError::PeerUnreachable(peer)));
                    return;
                }
                self.swarm
                    .behaviour_mut()
                    .mesh_rr
                    .send_request(&libp2p_peer, MeshRequest::Frame(frame));
                let _ = reply.send(Ok(()));
            }
            Command::ConnectedPeers { reply } => {
                let peers = self.connected.values().copied().collect();
                let _ = reply.send(peers);
            }
            Command::ListenAddrs { reply } => {
                let _ = reply.send(self.listen_addrs.clone());
            }
            Command::ReserveRelay {
                relay_peer,
                relay_address,
                reply,
            } => {
                let libp2p_relay_peer = match domain_peer_to_libp2p(relay_peer) {
                    Ok(p) => p,
                    Err(err) => {
                        let _ =
                            reply.send(Err(MeshTransportError::MalformedAddress(err.to_string())));
                        return;
                    }
                };
                let addr: Multiaddr = match relay_address.parse() {
                    Ok(addr) => addr,
                    Err(_) => {
                        let _ =
                            reply.send(Err(MeshTransportError::MalformedAddress(relay_address)));
                        return;
                    }
                };
                let circuit_addr = addr
                    .with(Protocol::P2p(libp2p_relay_peer))
                    .with(Protocol::P2pCircuit);
                match self.swarm.listen_on(circuit_addr) {
                    Ok(_) => {
                        let _ = reply.send(Ok(()));
                    }
                    Err(err) => {
                        let _ = reply.send(Err(MeshTransportError::Transport(err.to_string())));
                    }
                }
            }
        }
    }

    /// Shared by `Command::Connect` (a caller explicitly asking to connect)
    /// and `retry_disconnected_known_peers` (the background reconnect loop) —
    /// same dial, the only difference is who's waiting on `reply`.
    fn dial_peer(
        &mut self,
        libp2p_peer: Libp2pPeerId,
        peer: DomainPeerId,
        addr: Multiaddr,
        reply: oneshot::Sender<Result<(), MeshTransportError>>,
    ) {
        self.swarm
            .behaviour_mut()
            .kad
            .add_address(&libp2p_peer, addr.clone());
        // `PortUse::Reuse` (the default) is required for DCUtR hole-punching but
        // collides on one machine; POND_DEV_SAME_MACHINE_MESH=1 takes a fresh port.
        // `condition(Always)` is needed because a rejected handshake leaves the
        // connection open, so the default `DisconnectedAndNotDialing` never redials.
        let mut opts = DialOpts::peer_id(libp2p_peer)
            .condition(PeerCondition::Always)
            .addresses(vec![addr.clone()]);
        if same_machine_dev_mesh_enabled(
            std::env::var("POND_DEV_SAME_MACHINE_MESH").ok().as_deref(),
        ) {
            opts = opts.allocate_new_port();
        }
        match self.swarm.dial(opts.build()) {
            Ok(()) => {
                // `Always` above lets a retry start while an earlier dial
                // to the same peer is outstanding. `pending_connect` is
                // keyed by peer, so overwriting silently would leave the
                // earlier caller reporting "mesh swarm task has stopped".
                if let Some((_, stale_reply)) =
                    self.pending_connect.insert(libp2p_peer, (peer, reply))
                {
                    let _ = stale_reply.send(Err(MeshTransportError::Transport(
                        "superseded by a newer connect attempt to the same peer".to_string(),
                    )));
                }
            }
            Err(err) => {
                let _ = reply.send(Err(MeshTransportError::Transport(err.to_string())));
            }
        }
    }

    async fn handle_swarm_event(&mut self, event: SwarmEvent<MeshBehaviourEvent>) {
        match event {
            SwarmEvent::NewListenAddr { address, .. } => {
                self.listen_addrs.push(address);
            }
            SwarmEvent::ConnectionEstablished {
                peer_id, endpoint, ..
            } => {
                if endpoint.is_dialer() && self.pending_connect.contains_key(&peer_id) {
                    let request_id = self.swarm.behaviour_mut().mesh_rr.send_request(
                        &peer_id,
                        MeshRequest::Handshake(self.local_handshake_bytes.clone()),
                    );
                    self.pending_handshake.insert(request_id, peer_id);
                }
            }
            SwarmEvent::ConnectionClosed { peer_id, .. } => {
                self.connected.remove(&peer_id);
            }
            SwarmEvent::Behaviour(MeshBehaviourEvent::Identify(identify::Event::Received {
                info: identify::Info { observed_addr, .. },
                ..
            })) => {
                // A relay reservation is rejected without at least one known
                // external address to advertise — this is how we learn one
                // on a network with no manual external-address config.
                self.swarm.add_external_address(observed_addr);
            }
            SwarmEvent::OutgoingConnectionError {
                peer_id: Some(peer_id),
                error,
                ..
            } => {
                if let Some((_, reply)) = self.pending_connect.remove(&peer_id) {
                    let _ = reply.send(Err(MeshTransportError::Transport(error.to_string())));
                }
            }
            SwarmEvent::Behaviour(MeshBehaviourEvent::MeshRr(
                request_response::Event::Message { peer, message, .. },
            )) => self.handle_mesh_message(peer, message).await,
            SwarmEvent::Behaviour(MeshBehaviourEvent::MeshRr(
                request_response::Event::OutboundFailure {
                    request_id, error, ..
                },
            )) => {
                // Safety net so a dropped/reset connection (e.g. the peer
                // disconnecting right after a handshake rejection) always
                // resolves the caller's `connect()`/`send()` reply instead
                // of leaving it waiting forever.
                if let Some(libp2p_peer) = self.pending_handshake.remove(&request_id) {
                    if let Some((_, reply)) = self.pending_connect.remove(&libp2p_peer) {
                        let _ = reply.send(Err(MeshTransportError::Transport(error.to_string())));
                    }
                }
            }
            _ => {}
        }
    }

    async fn handle_mesh_message(
        &mut self,
        peer: Libp2pPeerId,
        message: request_response::Message<MeshRequest, MeshResponse>,
    ) {
        match message {
            request_response::Message::Request {
                request, channel, ..
            } => match request {
                MeshRequest::Handshake(bytes) => {
                    let verified = match self.verify_handshake(&bytes, peer) {
                        Some(claimed)
                            if Self::is_trusted(self.peer_directory.clone(), claimed).await =>
                        {
                            Some(claimed)
                        }
                        _ => None,
                    };
                    let response = if verified.is_some() {
                        // Answer with our OWN handshake so the dialer can hold
                        // us to the same standard it was just held to.
                        MeshResponse::HandshakeAccepted(self.local_handshake_bytes.clone())
                    } else {
                        MeshResponse::HandshakeRejected
                    };
                    let _ = self
                        .swarm
                        .behaviour_mut()
                        .mesh_rr
                        .send_response(channel, response);
                    // Don't force-disconnect on rejection: it races the outbound
                    // `send_response` and can drop the response, leaving the
                    // dialer's `connect()` waiting forever. Never promoting the
                    // peer into `connected` keeps it out of `recv()` anyway.
                    if let Some(domain_peer) = verified {
                        self.connected.insert(peer, domain_peer);
                    }
                }
                MeshRequest::Frame(bytes) => {
                    if let Some(domain_peer) = self.connected.get(&peer).copied() {
                        let _ = self.inbound_tx.send((domain_peer, bytes));
                    }
                    let _ = self
                        .swarm
                        .behaviour_mut()
                        .mesh_rr
                        .send_response(channel, MeshResponse::FrameAck);
                }
            },
            request_response::Message::Response {
                request_id,
                response,
            } => {
                if let Some(libp2p_peer) = self.pending_handshake.remove(&request_id) {
                    match response {
                        // Verify THEIR handshake before trusting the peer we
                        // dialled. Accepting a bare "yes" lets a peer running a
                        // different model, or none of this software, into
                        // `connected`, from where its frames reach `recv()`.
                        MeshResponse::HandshakeAccepted(their_handshake) => {
                            let ok = match self.verify_handshake(&their_handshake, libp2p_peer) {
                                Some(claimed) => {
                                    Self::is_trusted(self.peer_directory.clone(), claimed).await
                                }
                                None => false,
                            };
                            if let Some((domain_peer, reply)) =
                                self.pending_connect.remove(&libp2p_peer)
                            {
                                if ok {
                                    self.connected.insert(libp2p_peer, domain_peer);
                                    let _ = reply.send(Ok(()));
                                } else {
                                    let _ = reply.send(Err(MeshTransportError::Transport(
                                        "peer accepted our handshake but failed ours: harness/model \
                                         mismatch, bad signature, or not a trusted peer"
                                            .to_string(),
                                    )));
                                }
                            }
                        }
                        _ => {
                            if let Some((domain_peer, reply)) =
                                self.pending_connect.remove(&libp2p_peer)
                            {
                                let _ = reply
                                    .send(Err(MeshTransportError::PeerUnreachable(domain_peer)));
                            }
                        }
                    }
                }
                // Frame responses (FrameAck) need no handling — `send()`
                // reports dispatch, not delivery confirmation.
            }
        }
    }

    /// Decode and verify a `Handshake` from the connection with `conn_peer`:
    /// signature, harness/model pin, and that the claimed identity authenticated
    /// the connection. That binding is the security here — `signed_payload()` has
    /// no nonce, so it replays. Domain and libp2p ids are one key (`identity.rs`).
    fn verify_handshake(&self, bytes: &[u8], conn_peer: Libp2pPeerId) -> Option<DomainPeerId> {
        verify_handshake_bytes(bytes, conn_peer, self.harness_hash, self.model_hash)
    }

    /// Whether this Pond trusts `peer` at all. `PeerDirectory` is the trust
    /// circle; the handshake only proves the far side runs the same build and
    /// model. Costs one indexed SQLite read per handshake, not per frame. Takes
    /// the directory by `Arc` because `Swarm` is `Send` but not `Sync`.
    async fn is_trusted(directory: Arc<dyn PeerDirectory>, peer: DomainPeerId) -> bool {
        match directory.trust_scope_of(peer).await {
            Ok(scope) => scope.is_some(),
            // Fail CLOSED. A directory that cannot be read is not evidence of
            // trust, and this is the door to the household's compute.
            Err(err) => {
                tracing::warn!("mesh: peer directory unreadable, refusing {peer}: {err}");
                false
            }
        }
    }
}

/// The handshake check, as a free function so it can be tested without a
/// `Swarm`. Inside `EventLoop` the identity binding could be deleted with every
/// integration test still green: honest nodes always claim the identity that
/// authenticated the connection, so only a dishonest peer can catch its absence.
fn verify_handshake_bytes(
    bytes: &[u8],
    conn_peer: Libp2pPeerId,
    harness_hash: HarnessHash,
    model_hash: ModelHash,
) -> Option<DomainPeerId> {
    let handshake = Handshake::decode(bytes).ok()?;
    let peer_bytes: [u8; 32] = handshake.peer_id.clone().try_into().ok()?;
    let claimed_peer = DomainPeerId::from(peer_bytes);

    // The claimed identity must BE the connection's authenticated identity.
    if domain_peer_to_libp2p(claimed_peer).ok()? != conn_peer {
        return None;
    }

    let harness_bytes: [u8; 32] = handshake.harness_hash.clone().try_into().ok()?;
    if HarnessHash::from(harness_bytes) != harness_hash {
        return None;
    }
    let model_bytes: [u8; 32] = handshake.model_hash.clone().try_into().ok()?;
    if ModelHash::from(model_bytes) != model_hash {
        return None;
    }

    let signature: [u8; 64] = handshake.signature.clone().try_into().ok()?;
    let verified =
        pond_mesh_protocol::identity::verify(claimed_peer, &handshake.signed_payload(), &signature)
            .ok()?;
    verified.then_some(claimed_peer)
}

/// Truthiness check for `POND_DEV_SAME_MACHINE_MESH`, separated from the
/// env read so it's unit-testable.
fn same_machine_dev_mesh_enabled(value: Option<&str>) -> bool {
    matches!(value, Some("1") | Some("true") | Some("TRUE"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn harness() -> HarnessHash {
        HarnessHash::from([7u8; 32])
    }
    fn model() -> ModelHash {
        ModelHash::from([8u8; 32])
    }

    /// An honest peer's own handshake, on its own connection, is accepted.
    /// Without this the impersonation test below could pass because the
    /// function rejects everything.
    #[test]
    fn a_peer_presenting_its_own_handshake_on_its_own_connection_is_accepted() {
        let keypair = MeshKeypair::generate();
        let bytes = build_local_handshake(&keypair, harness(), model()).encode_to_vec();
        let conn = domain_peer_to_libp2p(keypair.peer_id()).unwrap();

        assert_eq!(
            verify_handshake_bytes(&bytes, conn, harness(), model()),
            Some(keypair.peer_id()),
        );
    }

    /// A REPLAYED handshake is refused, the attack the connection binding exists
    /// for. `signed_payload()` is `peer_id ‖ harness_hash ‖ model_hash` with no
    /// nonce, so every peer the victim dialled holds a valid copy. Only the
    /// connection identity cannot be forged without the victim's private key.
    #[test]
    fn a_replayed_handshake_cannot_impersonate_the_peer_that_signed_it() {
        let victim = MeshKeypair::generate();
        let attacker = MeshKeypair::generate();

        let stolen = build_local_handshake(&victim, harness(), model()).encode_to_vec();
        let attacker_conn = domain_peer_to_libp2p(attacker.peer_id()).unwrap();

        assert_eq!(
            verify_handshake_bytes(&stolen, attacker_conn, harness(), model()),
            None,
            "a captured handshake replayed over another peer's connection was \
             accepted -- every frame that peer sends would be attributed to the \
             victim",
        );
    }

    /// The hash pin still does its own job, in both fields, so the binding
    /// above did not quietly become the only check.
    #[test]
    fn a_mismatched_harness_or_model_is_refused_on_an_otherwise_honest_connection() {
        let keypair = MeshKeypair::generate();
        let conn = domain_peer_to_libp2p(keypair.peer_id()).unwrap();

        let wrong_harness =
            build_local_handshake(&keypair, HarnessHash::from([1u8; 32]), model()).encode_to_vec();
        assert_eq!(
            verify_handshake_bytes(&wrong_harness, conn, harness(), model()),
            None,
            "harness hash mismatch was accepted"
        );

        let wrong_model =
            build_local_handshake(&keypair, harness(), ModelHash::from([2u8; 32])).encode_to_vec();
        assert_eq!(
            verify_handshake_bytes(&wrong_model, conn, harness(), model()),
            None,
            "model hash mismatch was accepted"
        );
    }

    /// A forged signature is refused even when the claimed identity matches
    /// the connection — i.e. by the peer itself, tampering with its own blob.
    #[test]
    fn a_signature_that_does_not_verify_is_refused() {
        let keypair = MeshKeypair::generate();
        let conn = domain_peer_to_libp2p(keypair.peer_id()).unwrap();
        let forged =
            Handshake::new(keypair.peer_id(), harness(), model(), [0u8; 64]).encode_to_vec();

        assert_eq!(
            verify_handshake_bytes(&forged, conn, harness(), model()),
            None,
        );
    }

    /// Unset or unrecognised must fail closed to the DCUtR-compatible default.
    #[test]
    fn same_machine_dev_mesh_defaults_to_disabled() {
        assert!(!same_machine_dev_mesh_enabled(None));
        assert!(!same_machine_dev_mesh_enabled(Some("")));
        assert!(!same_machine_dev_mesh_enabled(Some("0")));
        assert!(!same_machine_dev_mesh_enabled(Some("false")));
        assert!(!same_machine_dev_mesh_enabled(Some("yes")));
    }

    #[test]
    fn same_machine_dev_mesh_recognises_truthy_values() {
        assert!(same_machine_dev_mesh_enabled(Some("1")));
        assert!(same_machine_dev_mesh_enabled(Some("true")));
        assert!(same_machine_dev_mesh_enabled(Some("TRUE")));
    }
}
