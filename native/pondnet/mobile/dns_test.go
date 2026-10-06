package mobile

import (
	"context"
	"io"
	"net"
	"net/netip"
	"slices"
	"strings"
	"sync"
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

// useResolvers installs servers for one test and restores the previous list after it.
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

// fakeDNS is a nameserver on 127.0.0.1 whose behaviour per transport is chosen by
// the test: which query types it answers over UDP and over TCP, whether it accepts
// TCP connections and then says nothing, and whether its UDP answers are truncated.
type fakeDNS struct {
	udpTypes, tcpTypes []uint16
	silentTCP          bool
	truncateUDP        bool
}

const (
	typeA    uint16 = 1
	typeAAAA uint16 = 28
)

// start serves f on one port for both transports and returns that address.
func (f fakeDNS) start(t *testing.T) netip.AddrPort {
	t.Helper()
	packet, err := net.ListenPacket("udp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	address := netip.MustParseAddrPort(packet.LocalAddr().String())
	listener, err := net.Listen("tcp", address.String())
	if err != nil {
		packet.Close()
		t.Skip("could not take the same port for tcp:", err)
	}
	var wg sync.WaitGroup
	wg.Add(2)
	go func() {
		defer wg.Done()
		buffer := make([]byte, 1500)
		for {
			read, from, err := packet.ReadFrom(buffer)
			if err != nil {
				return
			}
			if reply := f.reply(buffer[:read], f.udpTypes, f.truncateUDP); reply != nil {
				packet.WriteTo(reply, from)
			}
		}
	}()
	go func() {
		defer wg.Done()
		for {
			conn, err := listener.Accept()
			if err != nil {
				return
			}
			go f.serveTCP(conn)
		}
	}()
	t.Cleanup(func() { packet.Close(); listener.Close(); wg.Wait() })
	return address
}

func (f fakeDNS) serveTCP(conn net.Conn) {
	defer conn.Close()
	conn.SetDeadline(time.Now().Add(10 * time.Second))
	if f.silentTCP {
		io.Copy(io.Discard, conn)
		return
	}
	for {
		head := make([]byte, 2)
		if _, err := io.ReadFull(conn, head); err != nil {
			return
		}
		query := make([]byte, int(head[0])<<8|int(head[1]))
		if _, err := io.ReadFull(conn, query); err != nil {
			return
		}
		reply := f.reply(query, f.tcpTypes, false)
		if reply == nil {
			continue
		}
		conn.Write(append([]byte{byte(len(reply) >> 8), byte(len(reply))}, reply...))
	}
}

// reply answers query when its type is in answered: one A or AAAA record for an
// address query, an empty answer for anything else. It returns nil for a query it
// drops, which is what the router does to AAAA over UDP.
func (fakeDNS) reply(query []byte, answered []uint16, truncate bool) []byte {
	if len(query) < 12 {
		return nil
	}
	end := 12
	for end < len(query) && query[end] != 0 {
		end += int(query[end]) + 1
	}
	if end+5 > len(query) {
		return nil
	}
	question := query[12 : end+5]
	qtype := uint16(query[end+1])<<8 | uint16(query[end+2])
	if !slices.Contains(answered, qtype) {
		return nil
	}
	reply := []byte{query[0], query[1], 0x81, 0x80, 0, 1, 0, 0, 0, 0, 0, 0}
	if truncate {
		reply[2] |= 0x02
		return append(reply, question...)
	}
	var data []byte
	switch qtype {
	case typeA:
		data = []byte{192, 0, 2, 1}
	case typeAAAA:
		data = netip.MustParseAddr("2001:db8::1").AsSlice()
	}
	reply = append(reply, question...)
	if data != nil {
		reply[7] = 1
		reply = append(reply, 0xc0, 0x0c, byte(qtype>>8), byte(qtype), 0, 1, 0, 0, 0, 60, 0, byte(len(data)))
		reply = append(reply, data...)
	}
	return reply
}

// closedPort is an address with nothing listening on either transport.
func closedPort(t *testing.T) netip.AddrPort {
	t.Helper()
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	address := netip.MustParseAddrPort(listener.Addr().String())
	listener.Close()
	return address
}

// syncEvents records the event log from the racing goroutines.
type syncEvents struct {
	mu    sync.Mutex
	lines []string
}

func (s *syncEvents) Log(line string) { s.mu.Lock(); s.lines = append(s.lines, line); s.mu.Unlock() }

func (s *syncEvents) all() []string {
	s.mu.Lock()
	defer s.mu.Unlock()
	return slices.Clone(s.lines)
}

func recordEvents(t *testing.T) *syncEvents {
	t.Helper()
	events := new(syncEvents)
	SetEventLog(events)
	t.Cleanup(func() { SetEventLog(nil) })
	return events
}

// lookup resolves the test name through the node's resolver and reports how long
// it took. The name is fully qualified so no search domain of the machine running
// the test is tried.
func lookup(t *testing.T) ([]netip.Addr, time.Duration, error) {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 8*time.Second)
	defer cancel()
	started := time.Now()
	addresses, err := configuredResolver().LookupNetIP(ctx, "ip", "pond.example.")
	return addresses, time.Since(started), err
}

// resolvesBoth looks the test name up and requires both records, promptly.
func resolvesBoth(t *testing.T) {
	t.Helper()
	addresses, took, err := lookup(t)
	if err != nil {
		t.Fatalf("lookup failed after %s: %v", took, err)
	}
	if !slices.Contains(addresses, netip.MustParseAddr("192.0.2.1")) || !slices.Contains(addresses, netip.MustParseAddr("2001:db8::1")) {
		t.Fatalf("got %v, want the A and the AAAA record", addresses)
	}
	// Each case below used to wait out a whole query timeout, five seconds at
	// least. The point is not the exact figure but that no dead path costs one.
	if took > 2*time.Second {
		t.Fatalf("the lookup took %s", took)
	}
}

// The home router after the October 2026 power cut: A over UDP is answered, AAAA
// over UDP never is, and TCP answers both. Choosing UDP because it answered a probe
// made every lookup wait out its deadline for the AAAA answer.
func TestARouterThatDropsAaaaOverUdpStillResolvesAtOnce(t *testing.T) {
	events := recordEvents(t)
	useResolvers(t, fakeDNS{udpTypes: []uint16{typeA}, tcpTypes: []uint16{typeA, typeAAAA}}.start(t))
	resolvesBoth(t)
	if lines := events.all(); len(lines) != 0 {
		t.Fatalf("a lookup that was answered wrote to the event log: %q", lines)
	}
}

// Safaricom's first resolver: TCP connects at once and never answers; UDP answers.
func TestAResolverThatConnectsButNeverAnswersOverTcpCostsNothing(t *testing.T) {
	useResolvers(t, fakeDNS{udpTypes: []uint16{typeA, typeAAAA}, silentTCP: true}.start(t))
	resolvesBoth(t)
}

// The September 2026 router: nothing over UDP, everything over TCP.
func TestAServerAnsweringOnlyOverTcpResolves(t *testing.T) {
	useResolvers(t, fakeDNS{tcpTypes: []uint16{typeA, typeAAAA}}.start(t))
	resolvesBoth(t)
}

// Walking the list in order spent each lookup's budget on a dead first server.
func TestADeadFirstResolverDoesNotCostTheLookup(t *testing.T) {
	useResolvers(t, closedPort(t), fakeDNS{udpTypes: []uint16{typeA, typeAAAA}}.start(t))
	resolvesBoth(t)
}

// Two servers that each answer only part of what is asked still make a whole answer.
func TestEachQueryIsAnsweredByWhicheverServerCan(t *testing.T) {
	useResolvers(t, fakeDNS{udpTypes: []uint16{typeA}}.start(t), fakeDNS{tcpTypes: []uint16{typeAAAA}}.start(t))
	resolvesBoth(t)
}

func TestEveryResolverDeadIsAnErrorAndSaysSoOnce(t *testing.T) {
	events := recordEvents(t)
	useResolvers(t, closedPort(t))
	if addresses, took, err := lookup(t); err == nil {
		t.Fatalf("a lookup with no live server returned %v after %s", addresses, took)
	}
	lines := events.all()
	if len(lines) == 0 {
		t.Fatal("every server failed and nothing was written to the event log")
	}
	for _, line := range lines {
		if !strings.HasPrefix(line, "resolver: no server answered over udp or tcp: ") {
			t.Fatalf("unexpected event: %s", line)
		}
	}
}

// A truncated UDP answer loses to the whole answer over TCP.
func TestATruncatedUdpAnswerLosesToTheTcpOne(t *testing.T) {
	useResolvers(t, fakeDNS{udpTypes: []uint16{typeA}, tcpTypes: []uint16{typeA}, truncateUDP: true}.start(t))
	conn, err := dialResolver(context.Background(), "udp", "")
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()
	conn.SetDeadline(time.Now().Add(3 * time.Second))
	query := []byte{0xab, 0xcd, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 4, 'p', 'o', 'n', 'd', 0, 0, 1, 0, 1}
	if _, err := conn.Write(query); err != nil {
		t.Fatal(err)
	}
	reply := make([]byte, 512)
	read, err := conn.Read(reply)
	if err != nil {
		t.Fatal(err)
	}
	if reply[2]&0x02 != 0 || read <= len(query) {
		t.Fatalf("the truncated answer won: % x", reply[:read])
	}
}

// Go frames a query by the connection's type: a packet for UDP, a two-byte length
// for TCP, which it asks for after a truncated answer.
func TestTheConnectionFramesAsGoAsksForIt(t *testing.T) {
	useResolvers(t, fakeDNS{udpTypes: []uint16{typeA}, tcpTypes: []uint16{typeA}}.start(t))
	packet, err := dialResolver(context.Background(), "udp", "")
	if err != nil {
		t.Fatal(err)
	}
	packet.Close()
	if _, ok := packet.(net.PacketConn); !ok {
		t.Fatal("a udp dial returned a stream; Go would frame its query for tcp")
	}
	stream, err := dialResolver(context.Background(), "tcp", "")
	if err != nil {
		t.Fatal(err)
	}
	defer stream.Close()
	if _, ok := stream.(net.PacketConn); ok {
		t.Fatal("a tcp dial returned a packet connection")
	}
	stream.SetDeadline(time.Now().Add(3 * time.Second))
	query := []byte{0x12, 0x34, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 4, 'p', 'o', 'n', 'd', 0, 0, 1, 0, 1}
	// Written in two pieces, as a stream may be.
	framed := append([]byte{0, byte(len(query))}, query...)
	if _, err := stream.Write(framed[:5]); err != nil {
		t.Fatal(err)
	}
	if _, err := stream.Write(framed[5:]); err != nil {
		t.Fatal(err)
	}
	head := make([]byte, 2)
	if _, err := io.ReadFull(stream, head); err != nil {
		t.Fatal(err)
	}
	body := make([]byte, int(head[0])<<8|int(head[1]))
	if _, err := io.ReadFull(stream, body); err != nil {
		t.Fatal(err)
	}
	if body[0] != 0x12 || body[1] != 0x34 || len(body) <= len(query) {
		t.Fatalf("framed reply: % x", body)
	}
}

// An answer that never comes ends at the deadline Go set, not later.
func TestAnUnansweredQueryEndsAtTheDeadline(t *testing.T) {
	recordEvents(t)
	useResolvers(t, fakeDNS{silentTCP: true}.start(t))
	conn, err := dialResolver(context.Background(), "udp", "")
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()
	conn.SetDeadline(time.Now().Add(300 * time.Millisecond))
	query := []byte{0, 1, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 1}
	if _, err := conn.Write(query); err != nil {
		t.Fatal(err)
	}
	started := time.Now()
	if _, err := conn.Read(make([]byte, 512)); err == nil {
		t.Fatal("silence was read as an answer")
	}
	if took := time.Since(started); took > time.Second {
		t.Fatalf("the read outlived its deadline by %s", took)
	}
}
