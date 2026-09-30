package tailnetdial

import (
	"context"
	"errors"
	"io"
	"net"
	"net/http/httptest"
	"net/netip"
	"path/filepath"
	"testing"
	"time"

	"tailscale.com/ipn/store/mem"
	"tailscale.com/net/netns"
	"tailscale.com/tailcfg"
	"tailscale.com/tstest/integration"
	"tailscale.com/tstest/integration/testcontrol"
	"tailscale.com/tsnet"
	"tailscale.com/types/logger"
)

// These tests run two real tsnet nodes against an in-process control server and
// DERP relay on 127.0.0.1, so they need no network beyond loopback.

func startControl(t *testing.T, requireAuth bool) string {
	t.Helper()
	netns.SetEnabled(false)
	t.Cleanup(func() { netns.SetEnabled(true) })
	control := &testcontrol.Server{
		DERPMap:     integration.RunDERPAndSTUN(t, logger.Discard, "127.0.0.1"),
		DNSConfig:   &tailcfg.DNSConfig{Proxied: true},
		RequireAuth: requireAuth,
		Logf:        logger.Discard,
	}
	control.HTTPTestServer = httptest.NewUnstartedServer(control)
	control.HTTPTestServer.Start()
	t.Cleanup(control.HTTPTestServer.Close)
	return control.HTTPTestServer.URL
}

func newNode(t *testing.T, controlURL, hostname string) *tsnet.Server {
	t.Helper()
	server := &tsnet.Server{
		Dir:        filepath.Join(t.TempDir(), hostname),
		ControlURL: controlURL,
		Hostname:   hostname,
		Store:      new(mem.Store),
		Ephemeral:  true,
		Logf:       logger.Discard,
	}
	t.Cleanup(func() { server.Close() })
	return server
}

func startNode(t *testing.T, ctx context.Context, controlURL, hostname string) (*tsnet.Server, netip.Addr) {
	t.Helper()
	server := newNode(t, controlURL, hostname)
	status, err := server.Up(ctx)
	if err != nil {
		t.Fatalf("%s did not come up: %v", hostname, err)
	}
	waitForHomeDERP(t, ctx, server)
	return server, status.TailscaleIPs[0]
}

// waitForHomeDERP blocks until the node has a home DERP region and has heard from
// it; before that the relay drops peer DISCO frames and a fast dial races it.
func waitForHomeDERP(t *testing.T, ctx context.Context, server *tsnet.Server) {
	t.Helper()
	health := server.Sys().HealthTracker.Get()
	sock := server.Sys().MagicSock.Get()
	for {
		if report := sock.GetLastNetcheckReport(ctx); report != nil &&
			report.PreferredDERP != 0 &&
			!health.GetDERPRegionReceivedTime(report.PreferredDERP).IsZero() {
			return
		}
		select {
		case <-ctx.Done():
			t.Fatalf("no home DERP connection: %v", ctx.Err())
		case <-time.After(20 * time.Millisecond):
		}
	}
}

// hostListener is a TCP listener on the operating system's loopback, standing in
// for anything reachable through host routes: pond-server's own port, a LAN
// service, or a VPN the host happens to be connected to.
func hostListener(t *testing.T) (netip.AddrPort, <-chan struct{}) {
	t.Helper()
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { listener.Close() })
	accepted := make(chan struct{}, 1)
	go func() {
		for {
			conn, err := listener.Accept()
			if err != nil {
				return
			}
			conn.Close()
			select {
			case accepted <- struct{}{}:
			default:
			}
		}
	}()
	return netip.MustParseAddrPort(listener.Addr().String()), accepted
}

func TestTCPRefusesACancelledContext(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	if _, err := TCP(ctx, new(tsnet.Server), netip.MustParseAddrPort("100.64.0.1:443")); !errors.Is(err, context.Canceled) {
		t.Fatalf("got %v, want context.Canceled", err)
	}
}

func TestTCPRefusesANodeThatIsNotRunning(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), time.Minute)
	defer cancel()
	server := newNode(t, startControl(t, true), "unapproved")
	if err := server.Start(); err != nil {
		t.Fatal(err)
	}
	_, err := TCP(ctx, server, netip.MustParseAddrPort("100.64.0.1:443"))
	if err == nil || err.Error() != "embedded node is not running" {
		t.Fatalf("got %v, want the not-running refusal", err)
	}
}

// The property the package exists for: an address outside the tailnet is never
// reached through the host's routes. The canary proves the hazard is real on the
// pinned tsnet version, so if upstream ever removes the fallback this test fails
// and the package can be re-evaluated rather than silently kept.
func TestTCPNeverFallsBackToHostRoutes(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Minute)
	defer cancel()
	server, _ := startNode(t, ctx, startControl(t, false), "pond")
	target, accepted := hostListener(t)

	dialContext, dialCancel := context.WithTimeout(ctx, 2*time.Second)
	conn, err := TCP(dialContext, server, target)
	dialCancel()
	if err == nil {
		conn.Close()
		t.Fatalf("TCP connected to %s outside the tailnet", target)
	}
	select {
	case <-accepted:
		t.Fatalf("TCP reached the host listener at %s", target)
	case <-time.After(2 * time.Second):
	}

	canary, err := server.Dial(ctx, "tcp", target.String())
	if err != nil {
		t.Fatalf("canary: tsnet.Server.Dial no longer reaches host routes (%v); re-evaluate whether tailnetdial is still needed", err)
	}
	canary.Close()
	select {
	case <-accepted:
	case <-time.After(5 * time.Second):
		t.Fatal("canary: tsnet.Server.Dial connected but the host listener saw nothing")
	}
}

func TestTCPReachesATailnetPeer(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Minute)
	defer cancel()
	controlURL := startControl(t, false)
	server, _ := startNode(t, ctx, controlURL, "pond")
	peer, peerAddress := startNode(t, ctx, controlURL, "phone")

	listener, err := peer.Listen("tcp", ":8443")
	if err != nil {
		t.Fatal(err)
	}
	defer listener.Close()
	go func() {
		conn, err := listener.Accept()
		if err != nil {
			return
		}
		defer conn.Close()
		io.Copy(conn, conn)
	}()

	conn, err := TCP(ctx, server, netip.AddrPortFrom(peerAddress, 8443))
	if err != nil {
		t.Fatalf("TCP to the tailnet peer: %v", err)
	}
	defer conn.Close()
	conn.SetDeadline(time.Now().Add(30 * time.Second))
	if _, err := conn.Write([]byte{0x2a}); err != nil {
		t.Fatal(err)
	}
	echo := make([]byte, 1)
	if _, err := io.ReadFull(conn, echo); err != nil || echo[0] != 0x2a {
		t.Fatalf("round trip: got %v, %v", echo, err)
	}
}
