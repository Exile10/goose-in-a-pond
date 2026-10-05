package mobile

import (
	"context"
	"io"
	"net"
	"net/netip"
	"strings"
	"testing"
	"time"
)

func TestNativeResolverMetadata(t *testing.T) {
	result, err := parseResolvers(`["fe80::1%46","192.168.1.1","2001:4860:4860::8888"]`)
	if err != nil || len(result) != 3 {
		t.Fatal("valid resolvers rejected", err, result)
	}
	if result[0].Addr().Zone() != "46" {
		t.Fatal("a zoned link-local resolver lost its zone and would be undialable", result[0])
	}
	if result[1].String() != "192.168.1.1:53" {
		t.Fatal("resolver did not gain the DNS port", result[1])
	}
	// A link-local server without a zone cannot be dialled: resolving an interface
	// name needs netlink, which this platform denies to applications. Drop it
	// rather than reject the list and lose the usable servers with it.
	unzoned, err := parseResolvers(`["fe80::1","192.168.1.1"]`)
	if err != nil || len(unzoned) != 1 || unzoned[0].String() != "192.168.1.1:53" {
		t.Fatal("an unzoned link-local server was not dropped", err, unzoned)
	}

	empty, err := parseResolvers(`[]`)
	if err != nil || empty == nil {
		t.Fatal("an empty list must parse to a non-nil slice, not an error", err)
	}

	for _, raw := range []string{
		"null",
		"{}",
		`"192.168.1.1"`,
		strings.Repeat(" ", 4097),
		`["nonsense"]`,
		`["192.168.1.1","192.168.1.1"]`,
		`["0.0.0.0"]`,
		`["127.0.0.1"]`,
		`["224.0.0.251"]`,
		`["1.1.1.1","1.0.0.1","8.8.8.8","8.8.4.4","9.9.9.9","149.112.112.112","208.67.222.222","208.67.220.220","64.6.64.6"]`,
	} {
		if _, err := parseResolvers(raw); err == nil {
			t.Fatalf("invalid metadata accepted: %.80s", raw)
		}
	}
}

func TestResolversAreAbsentUntilInstalledAndSurviveAnEmptyUpdate(t *testing.T) {
	t.Cleanup(func() {
		resolverMu.Lock()
		resolvers = nil
		resolverMu.Unlock()
	})
	resolverMu.Lock()
	resolvers = nil
	resolverMu.Unlock()

	if len(configuredServers()) != 0 {
		t.Fatal("resolvers reported before any were installed")
	}
	if err := SetResolvers(`["192.168.1.1"]`); err != nil || len(configuredServers()) != 1 {
		t.Fatal("installing a resolver failed", err)
	}
	// The OS reports no active network for a moment across a reload. Keeping the
	// last good list is what stops every lookup failing in that window.
	if err := SetResolvers(`[]`); err != nil || len(configuredServers()) != 1 {
		t.Fatal("an empty update discarded the working resolvers", err)
	}
	if err := SetResolvers("nonsense"); err == nil {
		t.Fatal("malformed metadata accepted")
	}
	if len(configuredServers()) != 1 {
		t.Fatal("malformed metadata discarded the working resolvers")
	}
}

// The carrier case, which walking the list in order could not survive.
//
// Safaricom reports two resolvers for its LTE network and the FIRST one refuses
// DNS over TCP. Dialling them in sequence spent each lookup's budget on a server
// that would never answer, so the node resolved nothing on cellular while
// working perfectly on wifi.
func TestADeadFirstResolverDoesNotCostTheLookup(t *testing.T) {
	t.Cleanup(func() {
		resolverMu.Lock()
		resolvers = nil
		resolverMu.Unlock()
	})

	// A closed port stands in for the resolver that refuses TCP.
	answering, stopAnswering := tcpResponder(t, true)
	defer stopAnswering()

	dead, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	deadAddr := dead.Addr().(*net.TCPAddr)
	dead.Close() // nothing is listening there now

	live := answering.Addr().(*net.TCPAddr)
	resolverMu.Lock()
	resolvers = []netip.AddrPort{
		netip.AddrPortFrom(netip.MustParseAddr("127.0.0.1"), uint16(deadAddr.Port)),
		netip.AddrPortFrom(netip.MustParseAddr("127.0.0.1"), uint16(live.Port)),
	}
	resolverMu.Unlock()

	started := time.Now()
	conn, err := dialResolver(context.Background(), "udp", "")
	if err != nil {
		t.Fatal("the answering resolver was never reached:", err)
	}
	conn.Close()
	// Sequential dialling would have waited on the dead server first. The point
	// is not the exact number; it is that a dead server costs no wall clock.
	if elapsed := time.Since(started); elapsed > 2*time.Second {
		t.Fatalf("a dead first resolver cost %s of the lookup", elapsed)
	}
}

func TestEveryResolverDeadIsStillAnError(t *testing.T) {
	t.Cleanup(func() {
		resolverMu.Lock()
		resolvers = nil
		resolverMu.Unlock()
	})
	closed, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	addr := closed.Addr().(*net.TCPAddr)
	closed.Close()

	resolverMu.Lock()
	resolvers = []netip.AddrPort{netip.AddrPortFrom(netip.MustParseAddr("127.0.0.1"), uint16(addr.Port))}
	resolverMu.Unlock()

	if conn, err := dialResolver(context.Background(), "udp", ""); err == nil {
		conn.Close()
		t.Fatal("dialling a resolver that is not there reported success")
	}
}

// udpResponder answers any datagram with a minimal reply carrying the query's id,
// which is all udpAnswers checks for. It stands in for a router that serves DNS
// over UDP.
func udpResponder(t *testing.T) (netip.AddrPort, func()) {
	t.Helper()
	packet, err := net.ListenPacket("udp", "127.0.0.1:0")
	if err != nil {
		t.Fatal("could not listen on udp", err)
	}
	done := make(chan struct{})
	go func() {
		defer close(done)
		buffer := make([]byte, 512)
		for {
			read, from, err := packet.ReadFrom(buffer)
			if err != nil {
				return
			}
			if read < 2 {
				continue
			}
			reply := make([]byte, 12)
			reply[0], reply[1] = buffer[0], buffer[1]
			reply[2] = 0x81 // response, recursion desired
			if _, err := packet.WriteTo(reply, from); err != nil {
				return
			}
		}
	}()
	address := netip.MustParseAddrPort(packet.LocalAddr().String())
	return address, func() { packet.Close(); <-done }
}

func useResolvers(t *testing.T, servers ...netip.AddrPort) {
	t.Helper()
	resolverMu.Lock()
	previous := resolvers
	resolvers = servers
	resolverMu.Unlock()
	t.Cleanup(func() {
		resolverMu.Lock()
		resolvers = previous
		resolverMu.Unlock()
	})
}

// The failure this guards against: a router that answers DNS over UDP and ignores
// TCP. Dialling TCP only, the node could not resolve its coordinator at all on
// such a network, while every other application on it resolved normally.
func TestResolverFallsBackToUdpWhenTcpIsRefused(t *testing.T) {
	server, stop := udpResponder(t)
	defer stop()
	// Nothing is listening on TCP at that port, so the TCP half of the race fails.
	useResolvers(t, server)

	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	conn, err := dialResolver(ctx, "udp", "")
	if err != nil {
		t.Fatal("a server answering over udp was treated as unreachable", err)
	}
	defer conn.Close()
	// Go frames the query by the connection's type, so a UDP win has to hand back
	// something that is a PacketConn or the query goes out with TCP length prefixes.
	if _, ok := conn.(net.PacketConn); !ok {
		t.Fatal("udp winner returned a stream connection; Go would frame the query for tcp")
	}
}

// The behaviour that was already there and must survive: a server that answers
// only over TCP still resolves. This is the case the TCP-only dial was written for.
func TestResolverStillUsesTcpWhenUdpIsSilent(t *testing.T) {
	listener, stop := tcpResponder(t, true)
	defer stop()
	useResolvers(t, netip.MustParseAddrPort(listener.Addr().String()))

	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	conn, err := dialResolver(ctx, "udp", "")
	if err != nil {
		t.Fatal("a server answering over tcp was treated as unreachable", err)
	}
	defer conn.Close()
	if _, ok := conn.(net.PacketConn); ok {
		t.Fatal("tcp-only server produced a packet connection")
	}
}

func TestUdpProbeIsNotSatisfiedBySilence(t *testing.T) {
	// A UDP dial cannot fail, so an unanswered server must be rejected by the
	// exchange rather than by the dial. Without this, the race would always be
	// won instantly by a dead socket.
	packet, err := net.ListenPacket("udp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	address := netip.MustParseAddrPort(packet.LocalAddr().String())
	packet.Close() // nothing answers here now

	ctx, cancel := context.WithTimeout(context.Background(), 700*time.Millisecond)
	defer cancel()
	if udpAnswers(ctx, address) {
		t.Fatal("a server that never replied was reported as answering over udp")
	}
}

// tcpResponder accepts DNS-over-TCP connections. With answer set it replies to each
// framed query with a minimal header carrying the query's id; without it, it
// accepts and then says nothing, as the first resolver Safaricom reports does.
func tcpResponder(t *testing.T, answer bool) (net.Listener, func()) {
	t.Helper()
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal("could not listen on tcp", err)
	}
	done := make(chan struct{})
	go func() {
		defer close(done)
		for {
			conn, err := listener.Accept()
			if err != nil {
				return
			}
			go func(conn net.Conn) {
				defer conn.Close()
				if !answer {
					// Hold the connection open and silent until the client gives up.
					_, _ = io.Copy(io.Discard, conn)
					return
				}
				head := make([]byte, 2)
				if _, err := io.ReadFull(conn, head); err != nil {
					return
				}
				query := make([]byte, int(head[0])<<8|int(head[1]))
				if _, err := io.ReadFull(conn, query); err != nil || len(query) < 2 {
					return
				}
				reply := make([]byte, 14)
				reply[1] = 12
				reply[2], reply[3] = query[0], query[1]
				reply[4] = 0x81 // response, recursion desired
				_, _ = conn.Write(reply)
			}(conn)
		}
	}()
	return listener, func() { listener.Close(); <-done }
}

// The shape Safaricom's first resolver has: TCP connects at once and never
// answers, UDP answers. A connect beats any UDP answer, so taking it as proof
// handed Go a connection that hung for the whole lookup.
func TestAResolverThatConnectsButNeverAnswersOverTcpIsNotUsedForTcp(t *testing.T) {
	udp, stopUDP := udpResponder(t)
	defer stopUDP()
	// The silent TCP listener on the same port the UDP responder answers on.
	silent, err := net.Listen("tcp", udp.String())
	if err != nil {
		t.Skip("could not take the same port for tcp:", err)
	}
	defer silent.Close()
	go func() {
		for {
			conn, err := silent.Accept()
			if err != nil {
				return
			}
			go func() { defer conn.Close(); _, _ = io.Copy(io.Discard, conn) }()
		}
	}()
	useResolvers(t, udp)

	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	conn, err := dialResolver(ctx, "udp", "")
	if err != nil {
		t.Fatal("the server answering over udp was not used:", err)
	}
	defer conn.Close()
	if _, ok := conn.(net.PacketConn); !ok {
		t.Fatal("a tcp connection that never answers won the race")
	}
}

func TestTcpProbeIsNotSatisfiedByAConnect(t *testing.T) {
	listener, stop := tcpResponder(t, false)
	defer stop()
	ctx, cancel := context.WithTimeout(context.Background(), 700*time.Millisecond)
	defer cancel()
	if tcpAnswers(ctx, netip.MustParseAddrPort(listener.Addr().String())) {
		t.Fatal("a server that accepted and never answered was reported as answering over tcp")
	}
}

func TestTcpProbeIsSatisfiedByAnAnswer(t *testing.T) {
	listener, stop := tcpResponder(t, true)
	defer stop()
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	if !tcpAnswers(ctx, netip.MustParseAddrPort(listener.Addr().String())) {
		t.Fatal("a server answering over tcp was reported as silent")
	}
}
