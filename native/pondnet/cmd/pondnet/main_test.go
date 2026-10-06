package main

import (
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
