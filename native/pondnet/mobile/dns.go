package mobile

import (
	"context"
	"encoding/json"
	"errors"
	"net"
	"net/netip"
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
// exist on this platform and dials a resolver the operating system actually
// reported. Successive calls rotate, so Go's own retry reaches a different
// server rather than the same unreachable one.
func dialResolver(ctx context.Context, network, _ string) (net.Conn, error) {
	servers := configuredServers()
	if len(servers) == 0 {
		diagnose("resolver: no nameserver has been installed")
		return nil, errors.New("no resolver is configured")
	}
	// A plain dialer, deliberately. netns.NewDialer panics without a monitor, and
	// obtaining the live one would mean taking the lifecycle lock that a starting
	// node already holds. On this platform netns only applies the protect and
	// bind-to-network hooks, and neither is registered here.
	// Always TCP, whatever Go asked for. A home router commonly answers DNS over
	// TCP while ignoring UDP from a client it did not hand the lease to, and Go
	// only retries over TCP when a UDP answer comes back truncated, never when it
	// times out - so a UDP attempt here just burns the lookup's whole budget.
	// Returning a stream connection is also what tells Go to frame the query for
	// TCP: it picks framing by whether this conn implements net.PacketConn.
	_ = network

	// Every server at once, first one to answer wins.
	//
	// These used to be tried in order with three seconds each, and a carrier
	// showed why that is not good enough: Safaricom reports two resolvers and
	// the FIRST one refuses DNS over TCP. Every lookup spent its budget dialling
	// a server that would never answer before reaching the one that would, so
	// the node could not resolve its coordinator at all on cellular while
	// working perfectly on wifi.
	//
	// The file already warned about this shape for UDP -- "a UDP attempt here
	// just burns the lookup's whole budget" -- and then reintroduced it by
	// walking a list. Racing them costs one extra connection to a server that is
	// answering anyway, and removes a whole class of ordering luck.
	attempt, cancel := context.WithTimeout(ctx, 5*time.Second)
	defer cancel()

	type dialed struct {
		conn net.Conn
		err  error
	}
	results := make(chan dialed, len(servers))
	for _, server := range servers {
		go func(server netip.AddrPort) {
			dialer := net.Dialer{}
			conn, err := dialer.DialContext(attempt, "tcp", server.String())
			if err != nil {
				diagnose("resolver: dialing " + server.String() + " over tcp failed: " + err.Error())
			}
			results <- dialed{conn: conn, err: err}
		}(server)
	}

	var last error
	for range servers {
		select {
		case result := <-results:
			if result.err == nil {
				// Cancelling closes the losers' dials; any that already
				// succeeded are closed by the drain below.
				go func() {
					for range make([]struct{}, len(servers)-1) {
						if late := <-results; late.conn != nil {
							late.conn.Close()
						}
					}
				}()
				return result.conn, nil
			}
			last = result.err
		case <-ctx.Done():
			return nil, ctx.Err()
		}
	}
	if last != nil {
		return nil, last
	}
	return nil, errors.New("no configured resolver could be dialled")
}

// configuredResolver resolves through the OS-reported servers. PreferGo is
// required: it is what routes lookups into the dialer above instead of the
// platform resolver that cannot answer for this node.
func configuredResolver() *net.Resolver {
	return &net.Resolver{PreferGo: true, Dial: dialResolver}
}
