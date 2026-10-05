package mobile

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/netip"
	"os"
	"sync"
	"time"
)

// Resolvers are the nameservers the operating system reports for the active
// network. The native layer pushes them in; nothing here calls back into it.
//
// On Android the platform resolver does not answer for this node. Observed on a
// Galaxy A57: a lookup of the coordinator hostname hung, then fell through to
// Tailscale's bootstrap DNS, which knows nothing about a self-hosted coordinator
// and discloses its hostname to tailscale.com while asking. The node never
// fetched a control key and never logged in.
//
// Why the platform resolver fails there is not established - the cgo resolver is
// linked and Go prefers it on Android, and the same name resolves from a shell on
// the same device. This routes around that rather than explaining it.
//
// Push, not pull. An earlier build asked the native layer for the servers from
// inside the dial, and the Java round trip blocked long enough that the lookup
// context was already cancelled by the time it returned. A dial must touch only
// memory.
//
// Only server addresses cross this boundary. No query, answer or search domain is
// collected.

var resolverMu sync.RWMutex
var resolvers []netip.AddrPort

// SetResolvers installs the nameservers to query, as a JSON array of textual IP
// addresses. It takes no lifecycle lock: the native layer calls it from a network
// callback that must never block behind a starting or stopping node.
//
// An empty or unusable list is kept rather than applied: the operating system
// reports no active network for a moment while the app reloads or a link changes,
// and failing every lookup in that window would be worse than answering from the
// last good list.
func SetResolvers(encoded string) error {
	parsed, err := parseResolvers(encoded)
	if err != nil {
		return err
	}
	if len(parsed) == 0 {
		return nil
	}
	resolverMu.Lock()
	resolvers = parsed
	resolverMu.Unlock()
	return nil
}

// parseResolvers accepts a JSON array of textual IP addresses. A malformed entry
// rejects the whole list: a partially understood resolver set is worse than none,
// because the node would silently query the wrong network.
func parseResolvers(raw string) ([]netip.AddrPort, error) {
	invalid := errors.New("invalid native resolver metadata")
	if len(raw) > 4096 {
		return nil, invalid
	}
	var records []string
	if json.Unmarshal([]byte(raw), &records) != nil || records == nil || len(records) > 8 {
		return nil, invalid
	}
	result := make([]netip.AddrPort, 0, len(records))
	seen := map[string]bool{}
	for _, record := range records {
		address, err := netip.ParseAddr(record)
		// A resolver that is unspecified, loopback or multicast is never a server
		// this node can usefully query, and loopback in particular is the value Go
		// invents when it finds no configuration at all.
		if err != nil || !address.IsValid() || address.IsUnspecified() || address.IsLoopback() || address.IsMulticast() || seen[address.String()] {
			return nil, invalid
		}
		seen[address.String()] = true
		// Link-local servers are kept. On a home router the link-local address is
		// often the only one that answers, and it needs a zone to be dialable - a
		// numeric one, because resolving an interface name needs netlink, which
		// this platform denies to applications.
		if address.IsLinkLocalUnicast() && address.Zone() == "" {
			continue
		}
		result = append(result, netip.AddrPortFrom(address, 53))
	}
	return result, nil
}

func configuredServers() []netip.AddrPort {
	resolverMu.RLock()
	defer resolverMu.RUnlock()
	return resolvers
}

// dialResolver ignores the address Go derived from configuration that does not
// exist on this platform and hands Go a connection to every resolver the
// operating system reported. Each query written to it goes to every server over
// UDP and over TCP at once, and the first answer to that query is what Go reads.
//
// No transport and no server is chosen in advance, because none can be. Each of
// these was observed, and each defeated a rule that suited the one before:
//   - September 2026, a home router: its routable resolver answered nothing and
//     its link-local one answered only over TCP.
//   - Safaricom LTE: the first resolver accepts TCP connections and never answers
//     on them, while answering UDP in about 170 ms.
//   - October 2026, the same home router after a power cut: A queries are answered
//     over UDP in about 10 ms, AAAA queries over UDP never, and AAAA over TCP only
//     on the link-local address. A probe chose UDP for every query, so each lookup
//     waited out its whole deadline for the AAAA answer and the node could not
//     reach its coordinator on the home network.
//
// Racing every query costs a few extra packets to servers that are answering
// anyway, on a lookup the node makes a handful of times. Go frames a query by
// whether the connection is a net.PacketConn, so the connection is one for the
// UDP framing it asks for first, and a stream for the TCP framing it asks for
// when an answer comes back truncated.
//
// A dial here touches only memory: see SetResolvers.
func dialResolver(_ context.Context, network, _ string) (net.Conn, error) {
	servers := configuredServers()
	if len(servers) == 0 {
		diagnose("resolver: no nameserver has been installed")
		return nil, errors.New("no resolver is configured")
	}
	race := &racingResolver{servers: servers, answer: make(chan answered, 1), done: make(chan struct{})}
	if network == "tcp" || network == "tcp4" || network == "tcp6" {
		return &streamResolver{racingResolver: race}, nil
	}
	return &packetResolver{racingResolver: race}, nil
}

// exchangeTimeout bounds a query when Go has set no deadline on the connection.
const exchangeTimeout = 5 * time.Second

// racingResolver is the connection dialResolver returns. It holds one query at a
// time: a new Write abandons the race for the previous one.
type racingResolver struct {
	servers []netip.AddrPort

	mu       sync.Mutex
	deadline time.Time
	cancel   context.CancelFunc
	answer   chan answered
	closed   bool
	done     chan struct{}
}

// answered is how a race ends: the first answer to its query, or why there was none.
type answered struct {
	reply []byte
	err   error
}

// ask starts a race for query and returns at once; the outcome arrives on r.answer.
func (r *racingResolver) ask(query []byte) error {
	if len(query) < 12 {
		return errors.New("a DNS query is at least a header")
	}
	r.mu.Lock()
	if r.closed {
		r.mu.Unlock()
		return net.ErrClosed
	}
	if r.cancel != nil {
		r.cancel()
	}
	deadline := r.deadline
	if deadline.IsZero() {
		deadline = time.Now().Add(exchangeTimeout)
	}
	ctx, cancel := context.WithDeadline(context.Background(), deadline)
	r.cancel = cancel
	answer := make(chan answered, 1)
	r.answer = answer
	r.mu.Unlock()

	query = append([]byte(nil), query...)
	type result struct {
		reply []byte
		err   error
	}
	attempts := len(r.servers) * 2
	results := make(chan result, attempts)
	for _, server := range r.servers {
		go func(server netip.AddrPort) {
			reply, err := udpExchange(ctx, server, query)
			results <- result{reply, err}
		}(server)
		go func(server netip.AddrPort) {
			reply, err := tcpExchange(ctx, server, query)
			results <- result{reply, err}
		}(server)
	}
	go func() {
		defer cancel()
		var truncated []byte
		var failures []error
		for range attempts {
			got := <-results
			switch {
			case got.err != nil:
				failures = append(failures, got.err)
			case got.reply[2]&0x02 != 0:
				// A truncated UDP answer is kept only in case nothing better comes:
				// the same server's TCP answer is the whole one.
				if truncated == nil {
					truncated = got.reply
				}
			default:
				answer <- answered{reply: got.reply}
				return
			}
		}
		if truncated != nil {
			answer <- answered{reply: truncated}
			return
		}
		// Every server and transport failed. Saying so now, rather than at the
		// deadline, lets Go move on to its next attempt at once.
		failed := errors.Join(failures...)
		answer <- answered{err: failed}
		// The one line worth writing, unless the race was abandoned for a newer
		// query or a close.
		if !errors.Is(ctx.Err(), context.Canceled) {
			diagnose("resolver: no server answered over udp or tcp: " + failed.Error())
		}
	}()
	return nil
}

// wait returns the answer to the current query, or an error at the deadline.
func (r *racingResolver) wait() ([]byte, error) {
	r.mu.Lock()
	answer, deadline := r.answer, r.deadline
	r.mu.Unlock()
	var expired <-chan time.Time
	if !deadline.IsZero() {
		timer := time.NewTimer(time.Until(deadline))
		defer timer.Stop()
		expired = timer.C
	}
	select {
	case outcome := <-answer:
		return outcome.reply, outcome.err
	case <-expired:
		return nil, os.ErrDeadlineExceeded
	case <-r.done:
		return nil, net.ErrClosed
	}
}

func (r *racingResolver) Close() error {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.closed {
		return nil
	}
	r.closed = true
	if r.cancel != nil {
		r.cancel()
	}
	close(r.done)
	return nil
}

func (r *racingResolver) SetDeadline(t time.Time) error {
	r.mu.Lock()
	r.deadline = t
	r.mu.Unlock()
	return nil
}

func (r *racingResolver) SetReadDeadline(t time.Time) error { return r.SetDeadline(t) }

// SetWriteDeadline has nothing to bound: a Write only starts the race.
func (r *racingResolver) SetWriteDeadline(time.Time) error { return nil }

func (r *racingResolver) LocalAddr() net.Addr { return &net.UDPAddr{} }

func (r *racingResolver) RemoteAddr() net.Addr {
	return net.UDPAddrFromAddrPort(r.servers[0])
}

// packetResolver carries one DNS message per Write and per Read, as UDP does.
type packetResolver struct{ *racingResolver }

func (p *packetResolver) Write(query []byte) (int, error) {
	if err := p.ask(query); err != nil {
		return 0, err
	}
	return len(query), nil
}

func (p *packetResolver) Read(buffer []byte) (int, error) {
	reply, err := p.wait()
	if err != nil {
		return 0, err
	}
	if len(reply) > len(buffer) {
		// Too big for Go's UDP buffer: hand it the head marked truncated, and Go
		// asks again over TCP, which comes back here as a streamResolver.
		reply = append([]byte(nil), reply[:len(buffer)]...)
		reply[2] |= 0x02
	}
	return copy(buffer, reply), nil
}

func (p *packetResolver) ReadFrom(buffer []byte) (int, net.Addr, error) {
	read, err := p.Read(buffer)
	return read, p.RemoteAddr(), err
}

func (p *packetResolver) WriteTo(query []byte, _ net.Addr) (int, error) { return p.Write(query) }

// streamResolver carries DNS-over-TCP framing: a two-byte length before each message.
type streamResolver struct {
	*racingResolver
	pending []byte
	framed  []byte
}

func (s *streamResolver) Write(data []byte) (int, error) {
	s.framed = append(s.framed, data...)
	if len(s.framed) < 2 {
		return len(data), nil
	}
	size := int(s.framed[0])<<8 | int(s.framed[1])
	if len(s.framed) < 2+size {
		return len(data), nil
	}
	query := s.framed[2 : 2+size]
	s.framed = s.framed[2+size:]
	if err := s.ask(query); err != nil {
		return 0, err
	}
	return len(data), nil
}

func (s *streamResolver) Read(buffer []byte) (int, error) {
	if len(s.pending) == 0 {
		reply, err := s.wait()
		if err != nil {
			return 0, err
		}
		s.pending = append([]byte{byte(len(reply) >> 8), byte(len(reply))}, reply...)
	}
	read := copy(buffer, s.pending)
	s.pending = s.pending[read:]
	return read, nil
}

// udpExchange sends query to server over UDP and returns the reply carrying its id.
func udpExchange(ctx context.Context, server netip.AddrPort, query []byte) ([]byte, error) {
	conn, err := (&net.Dialer{}).DialContext(ctx, "udp", server.String())
	if err != nil {
		return nil, fmt.Errorf("%s udp: %w", server, err)
	}
	defer conn.Close()
	stop := context.AfterFunc(ctx, func() { conn.Close() })
	defer stop()
	if deadline, ok := ctx.Deadline(); ok {
		conn.SetDeadline(deadline)
	}
	if _, err := conn.Write(query); err != nil {
		return nil, fmt.Errorf("%s udp: %w", server, err)
	}
	buffer := make([]byte, 1232)
	for {
		read, err := conn.Read(buffer)
		if err != nil {
			return nil, fmt.Errorf("%s udp: %w", server, err)
		}
		// Anything else on the socket is not the answer to this query.
		if read >= 12 && buffer[0] == query[0] && buffer[1] == query[1] && buffer[2]&0x80 != 0 {
			return append([]byte(nil), buffer[:read]...), nil
		}
	}
}

// tcpExchange sends query to server over TCP and returns the reply carrying its id.
// A connection that is accepted and never answered fails at the deadline, as the
// first resolver Safaricom reports does.
func tcpExchange(ctx context.Context, server netip.AddrPort, query []byte) ([]byte, error) {
	conn, err := (&net.Dialer{}).DialContext(ctx, "tcp", server.String())
	if err != nil {
		return nil, fmt.Errorf("%s tcp: %w", server, err)
	}
	defer conn.Close()
	stop := context.AfterFunc(ctx, func() { conn.Close() })
	defer stop()
	if deadline, ok := ctx.Deadline(); ok {
		conn.SetDeadline(deadline)
	}
	framed := append([]byte{byte(len(query) >> 8), byte(len(query))}, query...)
	if _, err := conn.Write(framed); err != nil {
		return nil, fmt.Errorf("%s tcp: %w", server, err)
	}
	head := make([]byte, 2)
	if _, err := io.ReadFull(conn, head); err != nil {
		return nil, fmt.Errorf("%s tcp: %w", server, err)
	}
	reply := make([]byte, int(head[0])<<8|int(head[1]))
	if _, err := io.ReadFull(conn, reply); err != nil {
		return nil, fmt.Errorf("%s tcp: %w", server, err)
	}
	if len(reply) < 12 || reply[0] != query[0] || reply[1] != query[1] || reply[2]&0x80 == 0 {
		return nil, fmt.Errorf("%s tcp: the reply does not answer the query", server)
	}
	return reply, nil
}

// configuredResolver resolves through the OS-reported servers. PreferGo is
// required: it is what routes lookups into the dialer above instead of the
// platform resolver that cannot answer for this node.
func configuredResolver() *net.Resolver {
	return &net.Resolver{PreferGo: true, Dial: dialResolver}
}
