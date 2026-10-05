package pondnet

import (
	"bytes"
	"context"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"net/netip"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"tailscale.com/envknob"
	"tailscale.com/ipn/store/mem"
	"tailscale.com/net/netns"
	"tailscale.com/tailcfg"
	"tailscale.com/tsnet"
	"tailscale.com/tstest/integration"
	"tailscale.com/tstest/integration/testcontrol"
	"tailscale.com/types/logger"

	"github.com/Exile10/goose-in-a-pond/native/pondnet/internal/tailnetdial"
)

// These tests run real tsnet nodes against an in-process control server and DERP
// relay on 127.0.0.1, so they need no network beyond loopback.

// cacheControl is a test coordinator that can be made unreachable. While closed,
// every new HTTP request is refused; a node that already holds a control session
// keeps it, as a Pond with a live map would while a phone restarts.
type cacheControl struct {
	*testcontrol.Server
	URL     string
	closed  atomic.Bool
	refused atomic.Int32
}

func startCacheControl(t *testing.T) *cacheControl {
	t.Helper()
	netns.SetEnabled(false)
	t.Cleanup(func() { netns.SetEnabled(true) })
	c := &cacheControl{Server: &testcontrol.Server{
		DERPMap:   integration.RunDERPAndSTUN(t, logger.Discard, "127.0.0.1"),
		DNSConfig: &tailcfg.DNSConfig{Proxied: true},
		AllOnline: true,
		Logf:      logger.Discard,
	}}
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if c.closed.Load() {
			c.refused.Add(1)
			http.Error(w, "coordinator unreachable", http.StatusServiceUnavailable)
			return
		}
		c.Server.ServeHTTP(w, r)
	}))
	t.Cleanup(server.Close)
	c.Server.HTTPTestServer = server
	c.URL = server.URL
	return c
}

// lines records a node's backend log so a test can ask what the backend did.
type lines struct {
	mu  sync.Mutex
	all []string
}

func (l *lines) logf(format string, args ...any) {
	l.mu.Lock()
	l.all = append(l.all, fmt.Sprintf(format, args...))
	l.mu.Unlock()
}

func (l *lines) contains(text string) bool {
	l.mu.Lock()
	defer l.mu.Unlock()
	for _, line := range l.all {
		if strings.Contains(line, text) {
			return true
		}
	}
	return false
}

// persistentNode is a node whose identity and preferences survive a restart in
// dir, as a phone's do.
func persistentNode(t *testing.T, dir, controlURL, hostname string, log *lines) *tsnet.Server {
	t.Helper()
	server := &tsnet.Server{Dir: dir, ControlURL: controlURL, Hostname: hostname, Logf: log.logf, UserLogf: log.logf}
	t.Cleanup(func() { server.Close() })
	return server
}

// echoPond is an ephemeral peer that echoes on :4443, standing in for the Pond.
func echoPond(t *testing.T, ctx context.Context, controlURL string) netip.Addr {
	t.Helper()
	pond := &tsnet.Server{Dir: filepath.Join(t.TempDir(), "pond"), ControlURL: controlURL, Hostname: "pond",
		Store: new(mem.Store), Ephemeral: true, Logf: logger.Discard, UserLogf: logger.Discard}
	t.Cleanup(func() { pond.Close() })
	status, err := pond.Up(ctx)
	if err != nil {
		t.Fatalf("pond did not come up: %v", err)
	}
	listener, err := pond.Listen("tcp", ":4443")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { listener.Close() })
	go func() {
		for {
			conn, err := listener.Accept()
			if err != nil {
				return
			}
			go func() { defer conn.Close(); io.Copy(conn, conn) }()
		}
	}()
	return status.TailscaleIPs[0]
}

// await polls until ready reports true, failing the test at the deadline.
func await(t *testing.T, ctx context.Context, what string, ready func() bool) {
	t.Helper()
	for !ready() {
		select {
		case <-ctx.Done():
			t.Fatalf("%s: %v", what, ctx.Err())
		case <-time.After(50 * time.Millisecond):
		}
	}
}

// selfStatus reports the node's backend state, whether it sees peer, and
// whether control has granted it the network-map cache.
func selfStatus(t *testing.T, ctx context.Context, server *tsnet.Server, peer netip.Addr) (state string, seesPeer, granted bool) {
	t.Helper()
	client, err := server.LocalClient()
	if err != nil {
		t.Fatal(err)
	}
	status, err := client.Status(ctx)
	if err != nil {
		return "", false, false
	}
	for _, p := range status.Peer {
		for _, ip := range p.TailscaleIPs {
			seesPeer = seesPeer || ip == peer
		}
	}
	granted = status.Self != nil && status.Self.HasCap(tailcfg.NodeAttrCacheNetworkMaps)
	return status.BackendState, seesPeer, granted
}

// cachedFiles lists the files of every network-map cache under dir, the layout
// RemoveNetworkMapCache depends on.
func cachedFiles(t *testing.T, dir string) []string {
	t.Helper()
	files, err := filepath.Glob(filepath.Join(dir, profileDataDirectory, "*", netmapCacheDirectory, "*"))
	if err != nil {
		t.Fatal(err)
	}
	return files
}

func roundTrip(t *testing.T, ctx context.Context, server *tsnet.Server, pond netip.Addr) {
	t.Helper()
	conn, err := tailnetdial.TCP(ctx, server, netip.AddrPortFrom(pond, 4443))
	if err != nil {
		t.Fatalf("dial the pond: %v", err)
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

// A phone granted cache-network-maps writes its map to disk, and after a restart
// with the coordinator unreachable it runs from that map and reaches its Pond. A
// phone without the grant writes nothing and, restarted the same way, cannot run.
func TestGrantedNodeRestartsFromCachedNetworkMapWithoutControl(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Minute)
	defer cancel()
	control := startCacheControl(t)
	pond := echoPond(t, ctx, control.URL)

	grantedDir, plainDir := filepath.Join(t.TempDir(), "granted"), filepath.Join(t.TempDir(), "plain")
	grantedLog, plainLog := new(lines), new(lines)
	granted := persistentNode(t, grantedDir, control.URL, "granted", grantedLog)
	plain := persistentNode(t, plainDir, control.URL, "plain", plainLog)
	status, err := granted.Up(ctx)
	if err != nil {
		t.Fatalf("granted phone did not come up: %v", err)
	}
	if _, err := plain.Up(ctx); err != nil {
		t.Fatalf("plain phone did not come up: %v", err)
	}
	control.SetNodeCapMap(status.Self.PublicKey, tailcfg.NodeCapMap{tailcfg.NodeAttrCacheNetworkMaps: nil})

	// The backend writes the cache in the same step that installs the map, so once
	// status shows the grant the cache is on disk.
	await(t, ctx, "the grant reaches the phone", func() bool {
		_, sees, has := selfStatus(t, ctx, granted, pond)
		return sees && has
	})
	files := cachedFiles(t, grantedDir)
	if len(files) == 0 {
		t.Fatal("a granted phone wrote no network-map cache")
	}
	for _, file := range files {
		info, err := os.Stat(file)
		if err != nil {
			t.Fatal(err)
		}
		if info.Mode().Perm() != 0600 {
			t.Fatalf("%s is mode %v, want 0600", file, info.Mode().Perm())
		}
		data, err := os.ReadFile(file)
		if err != nil {
			t.Fatal(err)
		}
		if bytes.Contains(data, []byte("privkey:")) {
			t.Fatalf("%s holds a private key", file)
		}
	}
	await(t, ctx, "the plain phone sees the pond", func() bool {
		_, sees, _ := selfStatus(t, ctx, plain, pond)
		return sees
	})
	if files := cachedFiles(t, plainDir); len(files) != 0 {
		t.Fatalf("a phone without the grant cached its network map: %v", files)
	}
	roundTrip(t, ctx, granted, pond)

	granted.Close()
	plain.Close()
	control.closed.Store(true)

	restartedLog := new(lines)
	restarted := persistentNode(t, grantedDir, control.URL, "granted", restartedLog)
	if err := restarted.Start(); err != nil {
		t.Fatal(err)
	}
	await(t, ctx, "the restarted phone runs from its cache", func() bool {
		state, sees, _ := selfStatus(t, ctx, restarted, pond)
		return state == "Running" && sees
	})
	if !restartedLog.contains("loaded netmap from disk cache") {
		t.Fatal("the restarted phone is running, but not from the disk cache")
	}
	roundTrip(t, ctx, restarted, pond)
	if control.refused.Load() == 0 {
		t.Fatal("the restarted phone never asked the coordinator, so its absence proved nothing")
	}

	plainRestarted := persistentNode(t, plainDir, control.URL, "plain", new(lines))
	if err := plainRestarted.Start(); err != nil {
		t.Fatal(err)
	}
	wait, stop := context.WithTimeout(ctx, 5*time.Second)
	defer stop()
	for wait.Err() == nil {
		if state, _, _ := selfStatus(t, ctx, plainRestarted, pond); state == "Running" {
			t.Fatal("a phone with no cache ran without its coordinator; the cached case proves nothing")
		}
		time.Sleep(100 * time.Millisecond)
	}

	// Disabling remote access: the backend's own clear, then removal from disk.
	if err := (&Node{Server: restarted}).ClearNetworkMapCache(); err != nil {
		t.Fatal(err)
	}
	if files := cachedFiles(t, grantedDir); len(files) != 0 {
		t.Fatalf("the backend left its cache behind: %v", files)
	}
	restarted.Close()
	if err := RemoveNetworkMapCache(grantedDir); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(filepath.Join(grantedDir, "tailscaled.state")); err != nil {
		t.Fatalf("erasing the cache took the identity with it: %v", err)
	}
}

// What the Pond helper relies on: with TS_USE_CACHED_NETMAP off, a node the
// coordinator grants the cache to still writes nothing.
func TestKnobOffKeepsAGrantedNodeUncached(t *testing.T) {
	envknob.SetenvForTest(t, "TS_USE_CACHED_NETMAP", "false")
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Minute)
	defer cancel()
	control := startCacheControl(t)
	pond := echoPond(t, ctx, control.URL)
	dir := filepath.Join(t.TempDir(), "refusing")
	node := persistentNode(t, dir, control.URL, "refusing", new(lines))
	status, err := node.Up(ctx)
	if err != nil {
		t.Fatalf("node did not come up: %v", err)
	}
	control.SetNodeCapMap(status.Self.PublicKey, tailcfg.NodeCapMap{tailcfg.NodeAttrCacheNetworkMaps: nil})
	await(t, ctx, "the grant reaches the node", func() bool {
		_, sees, has := selfStatus(t, ctx, node, pond)
		return sees && has
	})
	if files := cachedFiles(t, dir); len(files) != 0 {
		t.Fatalf("a node with the cache switched off wrote it anyway: %v", files)
	}
}

func TestRemoveNetworkMapCacheLeavesIdentity(t *testing.T) {
	dir := t.TempDir()
	cache := filepath.Join(dir, profileDataDirectory, "a1b2", netmapCacheDirectory)
	if err := os.MkdirAll(cache, 0700); err != nil {
		t.Fatal(err)
	}
	for _, file := range []string{filepath.Join(cache, "73656c66"), filepath.Join(dir, "tailscaled.state")} {
		if err := os.WriteFile(file, []byte("{}"), 0600); err != nil {
			t.Fatal(err)
		}
	}
	if err := RemoveNetworkMapCache(dir); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(cache); !os.IsNotExist(err) {
		t.Fatalf("cache survived: %v", err)
	}
	if _, err := os.Stat(filepath.Join(dir, "tailscaled.state")); err != nil {
		t.Fatalf("identity removed: %v", err)
	}
	if err := RemoveNetworkMapCache(filepath.Join(dir, "never-started")); err != nil {
		t.Fatalf("a directory with no cache: %v", err)
	}
	if err := RemoveNetworkMapCache("relative"); err == nil {
		t.Fatal("a relative state directory was accepted")
	}
}
