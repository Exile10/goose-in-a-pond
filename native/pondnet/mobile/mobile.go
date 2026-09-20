// Package mobile is the gomobile binding boundary shared by Android and iOS.
package mobile

import (
	"encoding/json"
	"errors"
	"sync"

	"github.com/Exile10/goose-in-a-pond/native/pondnet"
)

var lock sync.Mutex
var node *pondnet.Node
var proxy *pondnet.Proxy

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
	return proxy.SetTargets(targets)
}

// Stop invalidates tunnels and releases the node without deleting its identity.
func Stop() {
	lock.Lock()
	defer lock.Unlock()
	if proxy != nil {
		proxy.Close()
		proxy = nil
	}
	if node != nil {
		node.Close()
		node = nil
	}
}
