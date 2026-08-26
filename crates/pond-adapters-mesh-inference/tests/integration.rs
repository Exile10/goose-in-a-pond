//! In-process, loopback-only integration tests for mesh inference (#132
//! Milestone 3). Two real `Libp2pMeshTransport` nodes on `127.0.0.1`, mocked
//! `PeerDirectory`/`CreditLedger`/`UsageTally` (trust-pin persistence and
//! settlement are separate milestones — not what this crate is proving).
//! Nothing here touches real hardware or the public network, so no test is
//! `#[ignore]`d.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;

use pond_adapters_mesh_inference::MeshInferenceService;
use pond_adapters_mesh_libp2p::{Libp2pMeshTransport, Libp2pMeshTransportConfig};
use pond_core::mesh::domain::hashes::{HarnessHash, ModelHash};
use pond_core::mesh::domain::millisats::Millisats;
use pond_core::mesh::domain::trust_scope::TrustScope;
use pond_core::mesh::mocks::mock_credit_ledger::MockCreditLedger;
use pond_core::mesh::mocks::mock_payment_rail::MockPaymentRail;
use pond_core::mesh::mocks::mock_peer_directory::MockPeerDirectory;
use pond_core::mesh::mocks::mock_usage_tally::MockUsageTally;
use pond_core::mesh::ports::credit_ledger::CreditLedger;
use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;
use pond_core::user_data::ports::settings::SettingsRepository;
use pond_core::mesh::ports::mesh_transport::MeshTransport;
use pond_core::mesh::ports::payment_rail::PaymentRail;
use pond_core::mesh::ports::peer_capability_query::PeerCapabilityQuery;
use pond_core::mesh::ports::peer_directory::PeerDirectory;
use pond_core::models::mocks::mock_provider::MockProvider;
use pond_core::models::ports::provider::{LlmProvider, StreamToken};
use pond_mesh_protocol::identity::MeshKeypair;

const PRODUCTION_LIKE_TIMEOUT: Duration = Duration::from_secs(5);
const SHORT_TEST_TIMEOUT: Duration = Duration::from_millis(200);

fn harness() -> HarnessHash {
    HarnessHash::from([1u8; 32])
}

fn model() -> ModelHash {
    ModelHash::from([2u8; 32])
}

/// The transport's own connection-level `PeerDirectory`, separate from any
/// `MeshInferenceService`'s (application-level trust) — returned alongside
/// the transport so a test can grant trust once both sides' peer ids are
/// known (`connect` does this).
async fn spawn_transport() -> (Arc<Libp2pMeshTransport>, Arc<MockPeerDirectory>) {
    let directory = Arc::new(MockPeerDirectory::new());
    let config = Libp2pMeshTransportConfig {
        listen_addr: "/ip4/127.0.0.1/tcp/0".parse().unwrap(),
        harness_hash: harness(),
        model_hash: model(),
        keypair: MeshKeypair::generate(),
        peer_directory: directory.clone(),
    };
    (
        Arc::new(Libp2pMeshTransport::new(config).await.unwrap()),
        directory,
    )
}

/// Poll `listen_addresses()` until at least one address is confirmed —
/// binding to `tcp/0` resolves the actual port asynchronously. Mirrors
/// `pond-adapters-mesh-libp2p`'s own integration test helper.
async fn wait_for_listen_address(node: &Libp2pMeshTransport) -> String {
    for _ in 0..200 {
        let addrs = node.listen_addresses().await.unwrap();
        if let Some(addr) = addrs.into_iter().find(|a| !a.contains("p2p-circuit")) {
            return addr;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("node never reported a listen address");
}

/// Grants `a` and `b` mutual trust in each other's *transport-level*
/// directory, then connects `a` to `b` and waits until both sides'
/// `connected_peers()` agrees — `b` (the listener) only learns about `a`
/// once the handshake request arrives, asynchronously. Trust is not
/// symmetric in the domain, so both directions are stated rather than
/// assumed (mirrors `pond-adapters-mesh-libp2p`'s own test helper).
async fn connect(
    a: &Libp2pMeshTransport,
    a_dir: &MockPeerDirectory,
    b: &Libp2pMeshTransport,
    b_dir: &MockPeerDirectory,
) {
    a_dir
        .add_trusted_peer(b.local_peer_id(), TrustScope::Circle)
        .await
        .unwrap();
    b_dir
        .add_trusted_peer(a.local_peer_id(), TrustScope::Circle)
        .await
        .unwrap();

    let b_addr = wait_for_listen_address(b).await;
    a.connect(b.local_peer_id(), b_addr).await.unwrap();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if b.connected_peers().await.unwrap() == vec![a.local_peer_id()] {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "b never saw a as connected"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn a_low_power_pond_is_served_by_a_trusted_peer_and_gets_correct_output() {
    let (a_transport, a_dir) = spawn_transport().await; // the lender
    let (b_transport, b_dir) = spawn_transport().await; // the borrower
    connect(&b_transport, &b_dir, &a_transport, &a_dir).await;

    // A serves with MockProvider (its own already-active local model, stood
    // in for a real ollama/llamafile/local instance).
    let a_service = MeshInferenceService::spawn(
        a_transport.clone(),
        Arc::new(MockPeerDirectory::new()),
        Arc::new(MockCreditLedger::new()),
        Arc::new(MockUsageTally::new()),
        Arc::new(MockSettingsRepository::new()),
        Arc::new(MockProvider::new()),
        PRODUCTION_LIKE_TIMEOUT,
        Duration::from_secs(15 * 60),
        None,
    );
    let _ = &a_service; // keeps the responder alive for the duration of the test

    // B only ever borrows in this test — its own backing provider is never
    // invoked, but the service still needs one to construct.
    let b_peer_directory = Arc::new(MockPeerDirectory::new());
    let b_credit_ledger = Arc::new(MockCreditLedger::new());
    b_peer_directory
        .add_trusted_peer(a_transport.local_peer_id(), TrustScope::Circle)
        .await
        .unwrap();
    b_credit_ledger
        .credit(a_transport.local_peer_id(), Millisats::new(1_000))
        .await
        .unwrap();
    let b_service = MeshInferenceService::spawn(
        b_transport.clone(),
        b_peer_directory,
        b_credit_ledger,
        Arc::new(MockUsageTally::new()),
        Arc::new(MockSettingsRepository::new()),
        Arc::new(MockProvider::new()),
        PRODUCTION_LIKE_TIMEOUT,
        Duration::from_secs(15 * 60),
        None,
    );

    let response = tokio::time::timeout(
        Duration::from_secs(5),
        b_service.provider().complete(
            "You are helpful.",
            vec![pond_core::models::domain::message::ChatMessage::user(
                "What's the weather?",
            )],
        ),
    )
    .await
    .expect("request timed out")
    .expect("request failed");

    // Exactly what MockProvider's canned response produces — proves the
    // request really crossed the mesh to A and the reply really came back,
    // not a local echo.
    assert_eq!(response.content, "Mock response to: What's the weather?");
}

#[tokio::test]
async fn stream_complete_yields_a_terminal_usage_token() {
    let (a_transport, a_dir) = spawn_transport().await;
    let (b_transport, b_dir) = spawn_transport().await;
    connect(&b_transport, &b_dir, &a_transport, &a_dir).await;

    let _a_service = MeshInferenceService::spawn(
        a_transport.clone(),
        Arc::new(MockPeerDirectory::new()),
        Arc::new(MockCreditLedger::new()),
        Arc::new(MockUsageTally::new()),
        Arc::new(MockSettingsRepository::new()),
        Arc::new(MockProvider::new()),
        PRODUCTION_LIKE_TIMEOUT,
        Duration::from_secs(15 * 60),
        None,
    );

    let b_peer_directory = Arc::new(MockPeerDirectory::new());
    let b_credit_ledger = Arc::new(MockCreditLedger::new());
    b_peer_directory
        .add_trusted_peer(a_transport.local_peer_id(), TrustScope::Circle)
        .await
        .unwrap();
    b_credit_ledger
        .credit(a_transport.local_peer_id(), Millisats::new(1_000))
        .await
        .unwrap();
    let b_usage_tally = Arc::new(MockUsageTally::new());
    let b_service = MeshInferenceService::spawn(
        b_transport.clone(),
        b_peer_directory,
        b_credit_ledger,
        b_usage_tally.clone(),
        Arc::new(MockSettingsRepository::new()),
        Arc::new(MockProvider::new()),
        PRODUCTION_LIKE_TIMEOUT,
        Duration::from_secs(15 * 60),
        None,
    );

    let provider = b_service.provider();
    let mut stream = provider.stream_complete(
        "sys",
        vec![pond_core::models::domain::message::ChatMessage::user("hi")],
    );

    let mut saw_text = false;
    let mut saw_usage = false;
    while let Some(item) = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("stream stalled")
    {
        match item.unwrap() {
            StreamToken::Text(_) => saw_text = true,
            StreamToken::Usage(_) => saw_usage = true,
        }
    }
    assert!(saw_text, "expected at least one text token");
    assert!(saw_usage, "expected a terminal usage token");
}

/// Streams far more chunks than any reasonable `max_tokens` cap —
/// `MockProvider` only yields a single Text chunk, which can never
/// exercise a mid-stream cutoff.
struct LongWindedProvider;

#[async_trait::async_trait]
impl LlmProvider for LongWindedProvider {
    async fn complete(
        &self,
        _system_prompt: &str,
        _messages: Vec<pond_core::models::domain::message::ChatMessage>,
    ) -> anyhow::Result<pond_core::models::domain::message::ChatMessage> {
        unreachable!("only stream_complete is exercised by this test")
    }

    fn model_name(&self) -> String {
        "long-winded-test-provider".to_string()
    }

    fn stream_complete<'a>(
        &'a self,
        _system_prompt: &'a str,
        _messages: Vec<pond_core::models::domain::message::ChatMessage>,
    ) -> pond_core::models::ports::provider::TokenStream<'a> {
        // 60 * 200 chars ≈ 3,000 estimated tokens — well past DEFAULT_MAX_TOKENS (2048).
        let chunks: Vec<anyhow::Result<StreamToken>> = (0..60)
            .map(|_| Ok(StreamToken::Text("x".repeat(200))))
            .collect();
        Box::pin(futures::stream::iter(chunks))
    }
}

/// The lender must stop at `request.max_tokens`, not stream unbounded.
#[tokio::test]
async fn lender_stops_at_max_tokens_instead_of_streaming_the_whole_response() {
    let (a_transport, a_dir) = spawn_transport().await; // the lender
    let (b_transport, b_dir) = spawn_transport().await; // the borrower
    connect(&b_transport, &b_dir, &a_transport, &a_dir).await;

    let _a_service = MeshInferenceService::spawn(
        a_transport.clone(),
        Arc::new(MockPeerDirectory::new()),
        Arc::new(MockCreditLedger::new()),
        Arc::new(MockUsageTally::new()),
        Arc::new(MockSettingsRepository::new()),
        Arc::new(LongWindedProvider),
        PRODUCTION_LIKE_TIMEOUT,
        Duration::from_secs(15 * 60),
        None,
    );

    let b_peer_directory = Arc::new(MockPeerDirectory::new());
    let b_credit_ledger = Arc::new(MockCreditLedger::new());
    b_peer_directory
        .add_trusted_peer(a_transport.local_peer_id(), TrustScope::Circle)
        .await
        .unwrap();
    b_credit_ledger
        .credit(a_transport.local_peer_id(), Millisats::new(1_000))
        .await
        .unwrap();
    let b_service = MeshInferenceService::spawn(
        b_transport.clone(),
        b_peer_directory,
        b_credit_ledger,
        Arc::new(MockUsageTally::new()),
        Arc::new(MockSettingsRepository::new()),
        Arc::new(MockProvider::new()),
        PRODUCTION_LIKE_TIMEOUT,
        Duration::from_secs(15 * 60),
        None,
    );

    let provider = b_service.provider();
    let mut stream = provider.stream_complete(
        "sys",
        vec![pond_core::models::domain::message::ChatMessage::user("hi")],
    );

    let mut text_chunks = 0u32;
    let mut usage_tokens = None;
    let mut saw_error = false;
    while let Some(item) = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("stream stalled")
    {
        match item {
            Ok(StreamToken::Text(_)) => text_chunks += 1,
            Ok(StreamToken::Usage(stats)) => usage_tokens = Some(stats.completion_tokens),
            Err(_) => saw_error = true,
        }
    }

    assert!(
        !saw_error,
        "hitting the cap is the borrower's own declared limit, not a failure"
    );
    assert!(
        text_chunks < 60,
        "expected the lender to stop before streaming all 60 chunks, got {text_chunks}"
    );
    let usage_tokens = usage_tokens.expect("expected a terminal usage token even when truncated");
    assert!(
        usage_tokens > 0,
        "truncated usage must still report real tokens sent, not zero"
    );
}

/// Borrowing must actually spend the credit balance down, once a rate is set.
#[tokio::test]
async fn borrowing_debits_the_credit_ledger_once_a_rate_is_set() {
    let (a_transport, a_dir) = spawn_transport().await; // the lender
    let (b_transport, b_dir) = spawn_transport().await; // the borrower
    connect(&b_transport, &b_dir, &a_transport, &a_dir).await;

    let _a_service = MeshInferenceService::spawn(
        a_transport.clone(),
        Arc::new(MockPeerDirectory::new()),
        Arc::new(MockCreditLedger::new()),
        Arc::new(MockUsageTally::new()),
        Arc::new(MockSettingsRepository::new()),
        Arc::new(MockProvider::new()),
        PRODUCTION_LIKE_TIMEOUT,
        Duration::from_secs(15 * 60),
        None,
    );

    let b_peer_directory = Arc::new(MockPeerDirectory::new());
    let b_credit_ledger = Arc::new(MockCreditLedger::new());
    b_peer_directory
        .add_trusted_peer(a_transport.local_peer_id(), TrustScope::Circle)
        .await
        .unwrap();
    b_credit_ledger
        .credit(a_transport.local_peer_id(), Millisats::new(1_000_000))
        .await
        .unwrap();
    let b_settings = Arc::new(MockSettingsRepository::new());
    b_settings
        .set_key("mesh_settlement_millisats_per_token", "5".to_string())
        .await
        .unwrap();
    let b_service = MeshInferenceService::spawn(
        b_transport.clone(),
        b_peer_directory,
        b_credit_ledger.clone(),
        Arc::new(MockUsageTally::new()),
        b_settings,
        Arc::new(MockProvider::new()),
        PRODUCTION_LIKE_TIMEOUT,
        Duration::from_secs(15 * 60),
        None,
    );

    let balance_before = b_credit_ledger
        .balance(a_transport.local_peer_id())
        .await
        .unwrap();

    let response = tokio::time::timeout(
        Duration::from_secs(5),
        b_service.provider().complete(
            "sys",
            vec![pond_core::models::domain::message::ChatMessage::user("hi")],
        ),
    )
    .await
    .expect("request timed out")
    .expect("request failed");
    assert!(!response.content.is_empty());

    let balance_after = b_credit_ledger
        .balance(a_transport.local_peer_id())
        .await
        .unwrap();
    assert!(
        balance_after.value() < balance_before.value(),
        "expected debit to spend down the balance: before={balance_before}, after={balance_after}"
    );
}

/// Lending refuses once the ceiling is crossed, then allows again once the
/// (short, test-only) window rolls over.
#[tokio::test]
async fn lend_window_refuses_once_the_ceiling_is_crossed_then_resets() {
    let (a_transport, a_dir) = spawn_transport().await; // the lender
    let (b_transport, b_dir) = spawn_transport().await; // the borrower
    connect(&b_transport, &b_dir, &a_transport, &a_dir).await;

    // The ceiling is the lender's own setting, not the borrower's.
    let a_settings = Arc::new(MockSettingsRepository::new());
    a_settings
        .set_key("mesh_lend_token_ceiling", "5".to_string())
        .await
        .unwrap();
    let _a_service = MeshInferenceService::spawn(
        a_transport.clone(),
        Arc::new(MockPeerDirectory::new()),
        Arc::new(MockCreditLedger::new()),
        Arc::new(MockUsageTally::new()),
        a_settings,
        Arc::new(MockProvider::new()),
        PRODUCTION_LIKE_TIMEOUT,
        // Short test window, but longer than MockProvider's 100ms per-request
        // delay so two requests don't roll it over on their own.
        Duration::from_secs(2),
        None,
    );

    let b_peer_directory = Arc::new(MockPeerDirectory::new());
    let b_credit_ledger = Arc::new(MockCreditLedger::new());
    b_peer_directory
        .add_trusted_peer(a_transport.local_peer_id(), TrustScope::Circle)
        .await
        .unwrap();
    b_credit_ledger
        .credit(a_transport.local_peer_id(), Millisats::new(1_000_000))
        .await
        .unwrap();
    let b_service = MeshInferenceService::spawn(
        b_transport.clone(),
        b_peer_directory,
        b_credit_ledger,
        Arc::new(MockUsageTally::new()),
        Arc::new(MockSettingsRepository::new()),
        Arc::new(MockProvider::new()),
        PRODUCTION_LIKE_TIMEOUT,
        Duration::from_secs(15 * 60),
        None,
    );

    let provider = b_service.provider();
    let ask = || {
        tokio::time::timeout(
            Duration::from_secs(5),
            provider.complete(
                "sys",
                vec![pond_core::models::domain::message::ChatMessage::user("hi")],
            ),
        )
    };

    // First reply is under the ceiling of 5 on its own — succeeds.
    ask().await.expect("request timed out").expect(
        "first request should be within the ceiling — MockProvider's reply is far under 5 tokens",
    );

    // Second pushes the window total over 5 — must be refused.
    let refused = ask().await.expect("request timed out");
    assert!(
        refused.is_err(),
        "expected the second request to be refused once the window's ceiling was crossed"
    );

    // A rolling throttle, not a permanent ban — succeeds again once the window rolls over.
    tokio::time::sleep(Duration::from_millis(2_200)).await;
    ask().await
        .expect("request timed out")
        .expect("expected the request to succeed again once the window rolled over");
}

#[tokio::test]
async fn insufficient_balance_is_refused_before_any_network_call() {
    let (a_transport, a_dir) = spawn_transport().await;
    let (b_transport, b_dir) = spawn_transport().await;
    connect(&b_transport, &b_dir, &a_transport, &a_dir).await;

    let b_peer_directory = Arc::new(MockPeerDirectory::new());
    b_peer_directory
        .add_trusted_peer(a_transport.local_peer_id(), TrustScope::Circle)
        .await
        .unwrap();
    // Deliberately never credited — balance stays zero.
    let b_service = MeshInferenceService::spawn(
        b_transport.clone(),
        b_peer_directory,
        Arc::new(MockCreditLedger::new()),
        Arc::new(MockUsageTally::new()),
        Arc::new(MockSettingsRepository::new()),
        Arc::new(MockProvider::new()),
        PRODUCTION_LIKE_TIMEOUT,
        Duration::from_secs(15 * 60),
        None,
    );

    let result = b_service
        .provider()
        .complete(
            "sys",
            vec![pond_core::models::domain::message::ChatMessage::user("hi")],
        )
        .await;

    let err = result.expect_err("zero-balance request must be refused");
    // NoPeerAvailable (not Timeout/Transport) proves peer selection failed
    // before any frame was ever sent — the hot-path budget has no network
    // round trip in it.
    assert!(
        err.to_string().contains("no trusted, connected, funded"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn a_trusted_but_unconnected_peer_is_skipped() {
    let (a_transport, _a_dir) = spawn_transport().await;
    let (b_transport, _b_dir) = spawn_transport().await;
    // Deliberately never connected.

    let b_peer_directory = Arc::new(MockPeerDirectory::new());
    let b_credit_ledger = Arc::new(MockCreditLedger::new());
    b_peer_directory
        .add_trusted_peer(a_transport.local_peer_id(), TrustScope::Circle)
        .await
        .unwrap();
    b_credit_ledger
        .credit(a_transport.local_peer_id(), Millisats::new(1_000))
        .await
        .unwrap();
    let b_service = MeshInferenceService::spawn(
        b_transport.clone(),
        b_peer_directory,
        b_credit_ledger,
        Arc::new(MockUsageTally::new()),
        Arc::new(MockSettingsRepository::new()),
        Arc::new(MockProvider::new()),
        PRODUCTION_LIKE_TIMEOUT,
        Duration::from_secs(15 * 60),
        None,
    );

    let result = b_service
        .provider()
        .complete(
            "sys",
            vec![pond_core::models::domain::message::ChatMessage::user("hi")],
        )
        .await;

    let err = result.expect_err("an unconnected peer must not be selected");
    assert!(
        err.to_string().contains("no trusted, connected, funded"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn a_malformed_inbound_frame_does_not_crash_the_service() {
    let (a_transport, a_dir) = spawn_transport().await;
    let (b_transport, b_dir) = spawn_transport().await;
    connect(&b_transport, &b_dir, &a_transport, &a_dir).await;

    // B's service is the one whose recv loop / dispatch must survive this —
    // it's the side about to receive garbage.
    let b_peer_directory = Arc::new(MockPeerDirectory::new());
    let b_credit_ledger = Arc::new(MockCreditLedger::new());
    b_peer_directory
        .add_trusted_peer(a_transport.local_peer_id(), TrustScope::Circle)
        .await
        .unwrap();
    b_credit_ledger
        .credit(a_transport.local_peer_id(), Millisats::new(1_000))
        .await
        .unwrap();
    let b_service = MeshInferenceService::spawn(
        b_transport.clone(),
        b_peer_directory,
        b_credit_ledger,
        Arc::new(MockUsageTally::new()),
        Arc::new(MockSettingsRepository::new()),
        Arc::new(MockProvider::new()),
        PRODUCTION_LIKE_TIMEOUT,
        Duration::from_secs(15 * 60),
        None,
    );
    // A's service serves the legitimate follow-up request.
    let _a_service = MeshInferenceService::spawn(
        a_transport.clone(),
        Arc::new(MockPeerDirectory::new()),
        Arc::new(MockCreditLedger::new()),
        Arc::new(MockUsageTally::new()),
        Arc::new(MockSettingsRepository::new()),
        Arc::new(MockProvider::new()),
        PRODUCTION_LIKE_TIMEOUT,
        Duration::from_secs(15 * 60),
        None,
    );

    // Bypass the wire encoding entirely — MeshTransport::send takes opaque
    // bytes, so this reaches B's MeshFrame::decode with garbage.
    a_transport
        .send(b_transport.local_peer_id(), vec![0xff, 0x00, 0x01])
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    // A subsequent, well-formed request still works — proves the garbage
    // frame was logged and dropped, not a panic that killed B's recv loop.
    let response = tokio::time::timeout(
        Duration::from_secs(5),
        b_service.provider().complete(
            "sys",
            vec![pond_core::models::domain::message::ChatMessage::user("hi")],
        ),
    )
    .await
    .expect("request timed out")
    .expect("request failed");
    assert_eq!(response.content, "Mock response to: hi");
}

#[tokio::test]
async fn stream_times_out_if_the_peer_never_replies() {
    let (a_transport, a_dir) = spawn_transport().await;
    let (b_transport, b_dir) = spawn_transport().await;
    connect(&b_transport, &b_dir, &a_transport, &a_dir).await;

    // Deliberately no service spawned on A's side — nothing will ever answer
    // B's InferenceRequest, so the chunk timeout has to fire.
    let b_peer_directory = Arc::new(MockPeerDirectory::new());
    let b_credit_ledger = Arc::new(MockCreditLedger::new());
    b_peer_directory
        .add_trusted_peer(a_transport.local_peer_id(), TrustScope::Circle)
        .await
        .unwrap();
    b_credit_ledger
        .credit(a_transport.local_peer_id(), Millisats::new(1_000))
        .await
        .unwrap();
    let b_service = MeshInferenceService::spawn(
        b_transport.clone(),
        b_peer_directory,
        b_credit_ledger,
        Arc::new(MockUsageTally::new()),
        Arc::new(MockSettingsRepository::new()),
        Arc::new(MockProvider::new()),
        SHORT_TEST_TIMEOUT,
        Duration::from_secs(15 * 60),
        None,
    );

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        b_service.provider().complete(
            "sys",
            vec![pond_core::models::domain::message::ChatMessage::user("hi")],
        ),
    )
    .await
    .expect("test itself timed out — the mesh inference timeout never fired");

    let err = result.expect_err("a silent peer must eventually time out");
    assert!(
        err.to_string().contains("timeout"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn requesting_an_invoice_from_a_peer_with_a_payment_rail_returns_its_invoice() {
    let (a_transport, a_dir) = spawn_transport().await; // the peer being asked to pay
    let (b_transport, b_dir) = spawn_transport().await; // the peer requesting an invoice
    connect(&b_transport, &b_dir, &a_transport, &a_dir).await;

    // A has a payment rail configured — it can answer InvoiceRequests.
    let a_service = MeshInferenceService::spawn(
        a_transport.clone(),
        Arc::new(MockPeerDirectory::new()),
        Arc::new(MockCreditLedger::new()),
        Arc::new(MockUsageTally::new()),
        Arc::new(MockSettingsRepository::new()),
        Arc::new(MockProvider::new()),
        PRODUCTION_LIKE_TIMEOUT,
        Duration::from_secs(15 * 60),
        Some(Arc::new(MockPaymentRail::new()) as Arc<dyn PaymentRail>),
    );
    let _ = &a_service; // keeps the responder alive for the duration of the test

    // B has no payment rail of its own — it only needs to *ask* for one.
    let b_service = MeshInferenceService::spawn(
        b_transport.clone(),
        Arc::new(MockPeerDirectory::new()),
        Arc::new(MockCreditLedger::new()),
        Arc::new(MockUsageTally::new()),
        Arc::new(MockSettingsRepository::new()),
        Arc::new(MockProvider::new()),
        PRODUCTION_LIKE_TIMEOUT,
        Duration::from_secs(15 * 60),
        None,
    );

    let invoice = tokio::time::timeout(
        Duration::from_secs(5),
        b_service.request_invoice(a_transport.local_peer_id(), Millisats::new(5_000)),
    )
    .await
    .expect("request timed out")
    .expect("request failed");
    assert!(
        invoice.starts_with("mock-invoice-"),
        "unexpected invoice: {invoice}"
    );
}

#[tokio::test]
async fn requesting_an_invoice_from_a_peer_with_no_payment_rail_gets_a_clean_error() {
    let (a_transport, a_dir) = spawn_transport().await; // the peer being asked to pay
    let (b_transport, b_dir) = spawn_transport().await; // the peer requesting an invoice
    connect(&b_transport, &b_dir, &a_transport, &a_dir).await;

    // A has no payment rail — Lightning off, same as the shipped default.
    let a_service = MeshInferenceService::spawn(
        a_transport.clone(),
        Arc::new(MockPeerDirectory::new()),
        Arc::new(MockCreditLedger::new()),
        Arc::new(MockUsageTally::new()),
        Arc::new(MockSettingsRepository::new()),
        Arc::new(MockProvider::new()),
        PRODUCTION_LIKE_TIMEOUT,
        Duration::from_secs(15 * 60),
        None,
    );
    let _ = &a_service;

    let b_service = MeshInferenceService::spawn(
        b_transport.clone(),
        Arc::new(MockPeerDirectory::new()),
        Arc::new(MockCreditLedger::new()),
        Arc::new(MockUsageTally::new()),
        Arc::new(MockSettingsRepository::new()),
        Arc::new(MockProvider::new()),
        PRODUCTION_LIKE_TIMEOUT,
        Duration::from_secs(15 * 60),
        None,
    );

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        b_service.request_invoice(a_transport.local_peer_id(), Millisats::new(5_000)),
    )
    .await
    .expect("request timed out");

    let err = result.expect_err("a peer with no payment rail must not fabricate an invoice");
    assert!(
        err.to_string().contains("no payment rail configured"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn querying_capabilities_reflects_the_peers_real_payment_rail_state() {
    let (a_transport, a_dir) = spawn_transport().await; // the peer being asked
    let (b_transport, b_dir) = spawn_transport().await; // the peer asking
    connect(&b_transport, &b_dir, &a_transport, &a_dir).await;

    // A has a payment rail configured — inference is always available (a
    // backing_provider is mandatory to construct the service at all).
    let a_service = MeshInferenceService::spawn(
        a_transport.clone(),
        Arc::new(MockPeerDirectory::new()),
        Arc::new(MockCreditLedger::new()),
        Arc::new(MockUsageTally::new()),
        Arc::new(MockSettingsRepository::new()),
        Arc::new(MockProvider::new()),
        PRODUCTION_LIKE_TIMEOUT,
        Duration::from_secs(15 * 60),
        Some(Arc::new(MockPaymentRail::new()) as Arc<dyn PaymentRail>),
    );
    let _ = &a_service;

    let b_service = MeshInferenceService::spawn(
        b_transport.clone(),
        Arc::new(MockPeerDirectory::new()),
        Arc::new(MockCreditLedger::new()),
        Arc::new(MockUsageTally::new()),
        Arc::new(MockSettingsRepository::new()),
        Arc::new(MockProvider::new()),
        PRODUCTION_LIKE_TIMEOUT,
        Duration::from_secs(15 * 60),
        None,
    );

    let capabilities = tokio::time::timeout(
        Duration::from_secs(5),
        b_service.capabilities_of(a_transport.local_peer_id()),
    )
    .await
    .expect("request timed out")
    .expect("request failed");

    assert!(capabilities.inference_available);
    assert!(capabilities.lightning_available);
}
