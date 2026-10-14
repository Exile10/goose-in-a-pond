package main

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"tailscale.com/envknob"
)

// The Pond helper must leave tailscale's network-map cache off, read the way the
// backend reads it.
func TestPondHelperRefusesCachedNetworkMaps(t *testing.T) {
	envknob.SetenvForTest(t, "TS_USE_CACHED_NETMAP", "")
	if !envknob.BoolDefaultTrue("TS_USE_CACHED_NETMAP") {
		t.Fatal("precondition: the cache is on by default")
	}
	refuseCachedNetworkMaps()
	if envknob.BoolDefaultTrue("TS_USE_CACHED_NETMAP") {
		t.Fatal("the Pond helper would start from a cached network map")
	}
}

// A device key that will not load blocks registration only when there is no invite to use.
func TestAnUnloadableDeviceFallsBackToTheInvite(t *testing.T) {
	directory := filepath.Join(t.TempDir(), "device")
	if err := os.Mkdir(directory, 0700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(directory, "key.json"), []byte(`{"seed":"bad"}`), 0600); err != nil {
		t.Fatal(err)
	}
	var warnings strings.Builder
	if _, err := registrationDevice(directory, "", &warnings); err == nil {
		t.Fatal("registered without an invite or a usable device certificate")
	}
	device, err := registrationDevice(directory, "invite-code", &warnings)
	if err != nil || device != nil {
		t.Fatalf("got %v, %v; want the invite alone", device, err)
	}
	if !strings.Contains(warnings.String(), "registering with the invite") {
		t.Fatalf("the fallback was not reported: %q", warnings.String())
	}
}
