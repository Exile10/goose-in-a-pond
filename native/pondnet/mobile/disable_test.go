package mobile

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

type recordedEvents struct{ lines []string }

func (r *recordedEvents) Log(line string) { r.lines = append(r.lines, line) }

// Disabling erases the cached network map even when the node is not running,
// keeps the identity, and reports a failure to erase in the event log.
func TestDisableErasesTheCachedNetworkMap(t *testing.T) {
	events := new(recordedEvents)
	SetEventLog(events)
	t.Cleanup(func() { SetEventLog(nil) })

	dir := t.TempDir()
	cache := filepath.Join(dir, "profile-data", "a1b2", "netmap-cache")
	if err := os.MkdirAll(cache, 0700); err != nil {
		t.Fatal(err)
	}
	for _, file := range []string{filepath.Join(cache, "73656c66"), filepath.Join(dir, "tailscaled.state")} {
		if err := os.WriteFile(file, []byte("{}"), 0600); err != nil {
			t.Fatal(err)
		}
	}
	if err := Disable(dir); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(cache); !os.IsNotExist(err) {
		t.Fatalf("the cached network map survived disabling: %v", err)
	}
	if _, err := os.Stat(filepath.Join(dir, "tailscaled.state")); err != nil {
		t.Fatalf("disabling removed the identity: %v", err)
	}
	if len(events.lines) != 0 {
		t.Fatalf("a clean disable logged %q", events.lines)
	}

	if err := Disable("relative"); err == nil {
		t.Fatal("an unusable state directory was reported as erased")
	}
	if len(events.lines) != 1 || !strings.Contains(events.lines[0], "cached network map was not erased") {
		t.Fatalf("the failure to erase was not recorded: %q", events.lines)
	}
}
