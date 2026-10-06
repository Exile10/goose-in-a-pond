// Package mobile is the gomobile binding boundary shared by Android and iOS.
package mobile

import (
	"context"
	"encoding/json"
	"errors"
	"net/netip"
	"sync"

	"github.com/Exile10/goose-in-a-pond/native/pondnet"
)

var lock sync.Mutex
var node *pondnet.Node
var proxy *pondnet.Proxy

// stopWatching ends the running node's ForgetNetworkMapWhenRefused.
var stopWatching context.CancelFunc

// Diagnostics receives redacted backend log lines through the binding boundary.
// The native layer installs one only for a debuggable build; a release build
// leaves backend logging discarded, as it is by default.
type Diagnostics interface {
	Log(line string)
}

var diagnosticsMu sync.RWMutex
var diagnostics Diagnostics

// SetDiagnostics installs or, with nil, clears the backend diagnostic sink. The
// binding layer keeps its own reference so it can report its own failures: a
// resolver that cannot be reached is invisible in the backend's log, which
// reports only that a name did not resolve.
func SetDiagnostics(sink Diagnostics) {
	diagnosticsMu.Lock()
	diagnostics = sink
	diagnosticsMu.Unlock()
	if sink == nil {
		pondnet.SetDiagnostics(nil)
		return
	}
	pondnet.SetDiagnostics(func(line string) { sink.Log(line) })
}

// diagnose reports a binding-layer event when an operator has enabled
// diagnostics. It is a no-op otherwise, like the backend sink.
// diagnose records something this package decided was worth saying: a handful
// of deliberate lines about why the node cannot do its job.
//
// Always recorded when an event sink is installed, and the native layer
// installs one unconditionally. These are not tailscale's backend log -- that
// is verbose, names addresses and keys, and stays behind `SetDiagnostics`.
// These are ours, there are a couple of them, and they are the difference
// between "the node did not connect" and "the first resolver refused TCP".
//
// Requiring somebody to find a switch and reproduce the failure before the
// reason is written down is how a field failure becomes undiagnosable, which is
// the thing the switch was meant to prevent.
func diagnose(line string) {
	eventMu.RLock()
	sink := events
	eventMu.RUnlock()
	if sink != nil {
		sink.Log(pondnet.Redact(line))
	}
}

var eventMu sync.RWMutex
var events Diagnostics

// SetEventLog installs the sink for this package's own diagnostic lines, or
// removes it with nil. Installed unconditionally by the native layer; see
// `diagnose`.
func SetEventLog(sink Diagnostics) {
	eventMu.Lock()
	events = sink
	eventMu.Unlock()
}

// Start owns one node in the application's private directory. It does not touch
// the separately installed Tailscale app or the operating system VPN settings.
func Start(directory, hostname, control string) error {
	lock.Lock()
	defer lock.Unlock()
	if node != nil {
		return errors.New("embedded networking is already started")
	}
	n, err := pondnet.Open(directory, hostname, control)
	if err != nil {
		return err
	}
	p, err := pondnet.NewProxy(n.Dial)
	if err != nil {
		n.Close()
		return err
	}
	node = n
	proxy = p
	ctx, cancel := context.WithCancel(context.Background())
	stopWatching = cancel
	go n.ForgetNetworkMapWhenRefused(ctx, diagnose)
	return nil
}

// Status returns connection/enrollment state, with no peer or account inventory.
func Status() (string, error) {
	lock.Lock()
	defer lock.Unlock()
	if node == nil {
		return `{"state":"Stopped","addresses":[]}`, nil
	}
	return node.SnapshotJSON()
}

// ProxyConfiguration is for native networking only; do not expose it to JS.
func ProxyConfiguration() (string, error) {
	lock.Lock()
	defer lock.Unlock()
	if proxy == nil {
		return "", errors.New("embedded networking is stopped")
	}
	b, err := json.Marshal(map[string]string{"address": proxy.Address(), "credential": proxy.Credential()})
	return string(b), err
}

// SetTargets atomically scopes the proxy to the paired Pond's tailnet addresses.
func SetTargets(encoded string) error {
	lock.Lock()
	defer lock.Unlock()
	if proxy == nil {
		return errors.New("embedded networking is stopped")
	}
	if len(encoded) > 4096 {
		return errors.New("target configuration is too large")
	}
	var targets []string
	if err := json.Unmarshal([]byte(encoded), &targets); err != nil {
		return err
	}
	// A tunnel to this node itself would reach its own listeners, not the Pond.
	ip4, ip6 := node.Server.TailscaleIPs()
	for _, target := range targets {
		if peer, err := netip.ParseAddrPort(target); err == nil && (peer.Addr() == ip4 || peer.Addr() == ip6) {
			return errors.New("a target is this device's own tailnet address")
		}
	}
	return proxy.SetTargets(targets)
}

// Stop invalidates tunnels and releases the node without deleting its identity.
// The cached network map stays, so the next Start can use it.
func Stop() {
	lock.Lock()
	defer lock.Unlock()
	stopLocked()
}

// Disable is Stop for remote access being switched off or the device removed:
// it also erases the network map the node cached in directory, its state
// directory. The identity stays, so enabling again needs no new enrollment, but
// the list of peers and the access rules a removed device last saw do not.
//
// The node is stopped whatever happens. A failure to erase is returned and
// recorded in the event log, never dropped: a cache left behind is the one
// thing this was meant to prevent.
func Disable(directory string) error {
	lock.Lock()
	defer lock.Unlock()
	if node != nil {
		// While the node runs, so the backend also drops what it holds in memory.
		// The removal below is what decides the outcome, so a refusal here is
		// recorded rather than returned.
		if err := node.ClearNetworkMapCache(); err != nil {
			diagnose("the running node did not clear its cached network map; erasing it from disk instead: " + err.Error())
		}
	}
	stopLocked()
	// After the node has stopped, so no map arriving in between survives.
	if err := pondnet.RemoveNetworkMapCache(directory); err != nil {
		diagnose("remote access was disabled, but its cached network map was not erased: " + err.Error())
		return err
	}
	return nil
}

func stopLocked() {
	if stopWatching != nil {
		stopWatching()
		stopWatching = nil
	}
	if proxy != nil {
		proxy.Close()
		proxy = nil
	}
	if node != nil {
		node.Close()
		node = nil
	}
}
