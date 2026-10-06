package privatefile

import (
	"errors"
	"os"
	"path/filepath"
	"syscall"
	"testing"
)

func TestOnlyPrivateRegularFilesOpen(t *testing.T) {
	dir := t.TempDir()
	good := filepath.Join(dir, "good")
	if err := os.WriteFile(good, []byte("secret"), 0o600); err != nil {
		t.Fatal(err)
	}
	f, err := Open(good, 64)
	if err != nil {
		t.Fatal(err)
	}
	f.Close()

	if _, err := Open(filepath.Join(dir, "absent"), 64); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("missing file: %v", err)
	}
	if _, err := Open(good, 3); !errors.Is(err, ErrNotPrivate) {
		t.Fatalf("oversized file: %v", err)
	}
	loose := filepath.Join(dir, "loose")
	os.WriteFile(loose, []byte("x"), 0o600)
	os.Chmod(loose, 0o640)
	if _, err := Open(loose, 64); !errors.Is(err, ErrNotPrivate) {
		t.Fatalf("group-readable file: %v", err)
	}
	link := filepath.Join(dir, "link")
	os.Symlink(good, link)
	if _, err := Open(link, 64); !errors.Is(err, ErrNotPrivate) {
		t.Fatalf("symlink: %v", err)
	}
	fifo := filepath.Join(dir, "fifo")
	if err := syscall.Mkfifo(fifo, 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := Open(fifo, 64); !errors.Is(err, ErrNotPrivate) {
		t.Fatalf("fifo: %v", err)
	}
	if _, err := Open(dir, 1<<20); !errors.Is(err, ErrNotPrivate) {
		t.Fatalf("directory: %v", err)
	}
}
