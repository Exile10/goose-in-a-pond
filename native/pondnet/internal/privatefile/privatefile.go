// Package privatefile opens files that hold keys or state, refusing anything that is not
// a private regular file of plausible size.
package privatefile

import (
	"errors"
	"fmt"
	"os"

	"golang.org/x/sys/unix"
)

// ErrNotPrivate is returned for a symlink, a non-regular file, a file readable or
// writable by group or others, or one larger than the caller's bound.
var ErrNotPrivate = errors.New("not a private regular file of plausible size")

// Open opens path for reading. The checks are made on the open descriptor, not on the
// path beforehand, so a file swapped in between a check and the open is never read:
// O_NOFOLLOW refuses a symlink and O_NONBLOCK keeps a planted FIFO from blocking.
// A missing file is reported as os.ErrNotExist.
func Open(path string, max int64) (*os.File, error) {
	fd, err := unix.Open(path, unix.O_RDONLY|unix.O_NOFOLLOW|unix.O_NONBLOCK|unix.O_CLOEXEC, 0)
	if err != nil {
		if errors.Is(err, unix.ENOENT) {
			return nil, fmt.Errorf("%s: %w", path, os.ErrNotExist)
		}
		if errors.Is(err, unix.ELOOP) {
			return nil, fmt.Errorf("%s: %w", path, ErrNotPrivate)
		}
		return nil, fmt.Errorf("%s: %w", path, err)
	}
	file := os.NewFile(uintptr(fd), path)
	info, err := file.Stat()
	if err != nil || !info.Mode().IsRegular() || info.Mode().Perm()&0o077 != 0 || info.Size() > max {
		file.Close()
		return nil, fmt.Errorf("%s: %w", path, ErrNotPrivate)
	}
	return file, nil
}
