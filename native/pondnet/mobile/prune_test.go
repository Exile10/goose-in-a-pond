package mobile

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"golang.org/x/sys/unix"
)

type pruneEvents struct{ lines []string }

func (p *pruneEvents) Log(line string) { p.lines = append(p.lines, line) }

func recordPruneEvents(t *testing.T) *pruneEvents {
	t.Helper()
	events := new(pruneEvents)
	SetEventLog(events)
	t.Cleanup(func() { SetEventLog(nil) })
	return events
}

// nodeDirectory lays out a stopped node's directory as the backend leaves it.
func nodeDirectory(t *testing.T, base, name string) string {
	t.Helper()
	dir := filepath.Join(base, name)
	cache := filepath.Join(dir, "profile-data", "a1b2", "netmap-cache")
	if err := os.MkdirAll(cache, 0700); err != nil {
		t.Fatal(err)
	}
	for _, file := range []string{"tailscaled.state", "node.lock", "initialized", filepath.Join("profile-data", "a1b2", "netmap-cache", "73656c66")} {
		if err := os.WriteFile(filepath.Join(dir, file), []byte("{}"), 0600); err != nil {
			t.Fatal(err)
		}
	}
	return dir
}

func exists(t *testing.T, path string) bool {
	t.Helper()
	_, err := os.Lstat(path)
	if err != nil && !os.IsNotExist(err) {
		t.Fatal(err)
	}
	return err == nil
}

// Every directory but the kept profile's goes, whatever is in it.
func TestPruneErasesEveryNodeDirectoryButTheKeptOne(t *testing.T) {
	events := recordPruneEvents(t)
	base := t.TempDir()
	kept := nodeDirectory(t, base, "1791176659988-s7bfxbtnkik")
	old := nodeDirectory(t, base, "1789909281353-qxhr18w1nsa")
	paired := nodeDirectory(t, base, "fe78fa4d-0f85-4d48-bfc9-cc0cfd9f7d45")
	empty := filepath.Join(base, "never-started")
	if err := os.Mkdir(empty, 0700); err != nil {
		t.Fatal(err)
	}

	if err := Prune(base, "1791176659988-s7bfxbtnkik"); err != nil {
		t.Fatal(err)
	}
	if !exists(t, filepath.Join(kept, "tailscaled.state")) {
		t.Fatal("the kept profile lost its identity")
	}
	for _, gone := range []string{old, paired, empty} {
		if exists(t, gone) {
			t.Fatalf("%s survived", gone)
		}
	}
	if len(events.lines) != 1 || events.lines[0] != "erased 3 superseded node identities" {
		t.Fatalf("event log: %q", events.lines)
	}
}

// A forgotten pairing keeps nothing; a second prune finds nothing and says nothing.
func TestPruneWithNoProfileErasesAllAndIsQuietWhenThereIsNothing(t *testing.T) {
	events := recordPruneEvents(t)
	base := t.TempDir()
	nodeDirectory(t, base, "a")
	nodeDirectory(t, base, "b")
	if err := Prune(base, ""); err != nil {
		t.Fatal(err)
	}
	if entries, err := os.ReadDir(base); err != nil || len(entries) != 0 {
		t.Fatalf("left %v, %v", entries, err)
	}
	if err := Prune(base, ""); err != nil {
		t.Fatal(err)
	}
	if err := Prune(filepath.Join(base, "missing"), ""); err != nil {
		t.Fatalf("a directory that was never created: %v", err)
	}
	if len(events.lines) != 1 {
		t.Fatalf("event log: %q", events.lines)
	}
}

// A directory whose node lock is held is a running node: it is refused, the
// refusal is reported, and the rest are still erased.
func TestPruneRefusesADirectoryWhoseNodeIsRunning(t *testing.T) {
	events := recordPruneEvents(t)
	base := t.TempDir()
	busy := nodeDirectory(t, base, "busy")
	stale := nodeDirectory(t, base, "stale")
	fd, err := unix.Open(filepath.Join(busy, "node.lock"), unix.O_RDWR, 0)
	if err != nil {
		t.Fatal(err)
	}
	defer unix.Close(fd)
	if err := unix.Flock(fd, unix.LOCK_EX|unix.LOCK_NB); err != nil {
		t.Fatal(err)
	}

	err = Prune(base, "")
	if err == nil || !strings.Contains(err.Error(), "busy: its node is running") {
		t.Fatalf("got %v", err)
	}
	if !exists(t, filepath.Join(busy, "tailscaled.state")) {
		t.Fatal("a running node's directory was erased")
	}
	if exists(t, stale) {
		t.Fatal("one refusal stopped the others being erased")
	}
	if len(events.lines) != 2 || !strings.HasPrefix(events.lines[1], "superseded node identities were not all erased") {
		t.Fatalf("event log: %q", events.lines)
	}
}

// A link is removed, not followed, so nothing outside the directory is touched.
func TestPruneRemovesALinkWithoutFollowingIt(t *testing.T) {
	recordPruneEvents(t)
	base, outside := t.TempDir(), t.TempDir()
	target := nodeDirectory(t, outside, "elsewhere")
	if err := os.Symlink(target, filepath.Join(base, "link")); err != nil {
		t.Fatal(err)
	}
	if err := Prune(base, ""); err != nil {
		t.Fatal(err)
	}
	if exists(t, filepath.Join(base, "link")) {
		t.Fatal("the link survived")
	}
	if !exists(t, filepath.Join(target, "tailscaled.state")) {
		t.Fatal("pruning followed a link out of the directory")
	}
}

func TestPruneRejectsUnsafeArguments(t *testing.T) {
	recordPruneEvents(t)
	base := t.TempDir()
	dir := nodeDirectory(t, base, "a")
	for _, call := range []struct{ base, keep string }{{"relative", ""}, {base, "../a"}, {base, "a/b"}, {base, strings.Repeat("a", 101)}} {
		if err := Prune(call.base, call.keep); err == nil {
			t.Fatalf("Prune(%q, %q) was accepted", call.base, call.keep)
		}
	}
	if !exists(t, dir) {
		t.Fatal("a rejected call erased something")
	}
}
