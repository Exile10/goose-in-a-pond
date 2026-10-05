package mobile

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"regexp"

	"golang.org/x/sys/unix"
)

// profilePattern is the profile name the native layers accept for a node directory.
var profilePattern = regexp.MustCompile(`^[A-Za-z0-9_-]{1,100}$`)

// Prune erases every node directory under base, the application's private
// directory of nodes, except the one named keep. An empty keep keeps none.
//
// Each pairing runs its node in its own directory, and a directory holds that
// node's private key and machine key. The companion keeps one pairing at a time,
// so once it activates a profile every other directory belongs to a pairing that
// was replaced or forgotten, whose identity nothing will use again. Left alone they
// accumulate, one per pairing, each with keys a coordinator may still accept.
//
// The running node's directory is never touched, and neither is any directory
// whose node lock is held, which is how a node in use is recognised whatever
// this package believes. Other entries are erased whatever their type; a
// symbolic link is removed, never followed. Every failure is returned and
// written to the event log, and the number erased is written there too.
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
	erased := 0
	for _, entry := range entries {
		path := filepath.Join(base, entry.Name())
		if entry.Name() == keep || path == running {
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
	if err := errors.Join(failures...); err != nil {
		diagnose("superseded node identities were not all erased: " + err.Error())
		return err
	}
	return nil
}

// eraseNodeDirectory removes path, holding its node lock while it does so that
// no node can start in it halfway through. A directory whose lock is already
// held belongs to a running node and is refused.
func eraseNodeDirectory(path string) error {
	info, err := os.Lstat(path)
	if err != nil {
		return err
	}
	if !info.IsDir() {
		return os.Remove(path)
	}
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
