package mobile

import (
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"regexp"

	"golang.org/x/sys/unix"
)

// profilePattern is the profile name the native layers accept for a node directory.
var profilePattern = regexp.MustCompile(`^[A-Za-z0-9_-]{1,100}$`)

// nodeMarkers are the files a node leaves in its directory: pondnet.Open creates
// node.lock and initialized, and its identity store writes tailscaled.state.
var nodeMarkers = []string{"node.lock", "initialized", "tailscaled.state"}

// Prune erases every node directory under base, the application's private
// directory of nodes, except the one named keep. An empty keep keeps none.
//
// Each pairing runs its node in its own directory, and a directory holds that
// node's private key and machine key. The companion keeps one pairing at a time,
// so once it activates a profile every other directory belongs to a pairing that
// was replaced or forgotten, whose identity nothing will use again. Left alone they
// accumulate, one per pairing, each with keys a coordinator may still accept.
//
// Only node directories are erased: an entry named as a profile is, that is a
// directory itself rather than a link to one, and that holds a file a node
// writes or nothing at all, which is what a start that failed straight after
// creating it leaves. Anything else under base is left alone and counted in the
// event log, since it is not this package's to erase.
//
// The running node's directory is never touched, and neither is any directory
// whose node lock is held, which is how a node in use is recognised whatever
// this package believes. Every failure is returned and written to the event log,
// and the number erased is written there too.
func Prune(base, keep string) error {
	if !filepath.IsAbs(base) {
		return errors.New("prune node directories: the directory must be absolute")
	}
	if keep != "" && !profilePattern.MatchString(keep) {
		return errors.New("prune node directories: invalid profile name")
	}
	lock.Lock()
	defer lock.Unlock()
	running := ""
	if node != nil {
		running = filepath.Clean(node.Server.Dir)
	}
	entries, err := os.ReadDir(base)
	if errors.Is(err, os.ErrNotExist) {
		return nil
	}
	if err != nil {
		diagnose("superseded node identities could not be listed: " + err.Error())
		return fmt.Errorf("prune node directories: %w", err)
	}
	var failures []error
	erased, left := 0, 0
	for _, entry := range entries {
		path := filepath.Join(base, entry.Name())
		if entry.Name() == keep || path == running {
			continue
		}
		if !profilePattern.MatchString(entry.Name()) {
			left++
			continue
		}
		isNode, err := isNodeDirectory(path)
		if err != nil {
			failures = append(failures, fmt.Errorf("prune node directory %s: %w", entry.Name(), err))
			continue
		}
		if !isNode {
			left++
			continue
		}
		if err := eraseNodeDirectory(path); err != nil {
			failures = append(failures, fmt.Errorf("prune node directory %s: %w", entry.Name(), err))
			continue
		}
		erased++
	}
	if erased > 0 {
		diagnose(fmt.Sprintf("erased %d superseded node identities", erased))
	}
	if left > 0 {
		diagnose(fmt.Sprintf("left %d entries that are not node directories", left))
	}
	if err := errors.Join(failures...); err != nil {
		diagnose("superseded node identities were not all erased: " + err.Error())
		return err
	}
	return nil
}

// isNodeDirectory reports whether path is a directory a node made: a directory
// itself, not a link to one, holding one of nodeMarkers or nothing at all.
func isNodeDirectory(path string) (bool, error) {
	info, err := os.Lstat(path)
	if err != nil {
		return false, err
	}
	if !info.IsDir() {
		return false, nil
	}
	for _, marker := range nodeMarkers {
		_, err := os.Lstat(filepath.Join(path, marker))
		if err == nil {
			return true, nil
		}
		if !errors.Is(err, os.ErrNotExist) {
			return false, err
		}
	}
	dir, err := os.Open(path)
	if err != nil {
		return false, err
	}
	defer dir.Close()
	// Something other than a node's files is in it, or reading it failed.
	if _, err := dir.Readdirnames(1); !errors.Is(err, io.EOF) {
		return false, err
	}
	return true, nil
}

// eraseNodeDirectory removes path, a node directory, holding its node lock while
// it does so that no node can start in it halfway through. A directory whose lock
// is already held belongs to a running node and is refused.
func eraseNodeDirectory(path string) error {
	fd, err := unix.Open(filepath.Join(path, "node.lock"), unix.O_RDWR|unix.O_NOFOLLOW|unix.O_CLOEXEC, 0)
	switch {
	case errors.Is(err, unix.ENOENT):
	case err != nil:
		return fmt.Errorf("open its node lock: %w", err)
	default:
		defer unix.Close(fd)
		if err := unix.Flock(fd, unix.LOCK_EX|unix.LOCK_NB); err != nil {
			return errors.New("its node is running")
		}
	}
	return os.RemoveAll(path)
}
