package mobile

import (
	"strings"
	"testing"
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
