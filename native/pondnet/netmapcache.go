package pondnet

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"time"
)

// The backend keeps a node's cached network map, when the coordinator grants it
// one, in <state>/profile-data/<profile>/netmap-cache. The cache tests pin this
// layout, so a tailscale upgrade that moves it fails them rather than leaving
// RemoveNetworkMapCache deleting nothing.
const (
	profileDataDirectory = "profile-data"
	netmapCacheDirectory = "netmap-cache"
)

// ClearNetworkMapCache asks the running backend to discard the network map it
// cached on disk and the cache state it holds in memory.
//
// It is the backend's own operation, so it does not depend on where the cache
// lives. It is not sufficient on its own: the backend does not report a failure
// to delete through this API, and a map arriving afterwards would be written
// again. Follow it with Close and RemoveNetworkMapCache.
func (n *Node) ClearNetworkMapCache() error {
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	client, err := n.Server.LocalClient()
	if err != nil {
		return fmt.Errorf("clear cached network map: %w", err)
	}
	if err := client.DebugAction(ctx, "clear-netmap-cache"); err != nil {
		return fmt.Errorf("clear cached network map: %w", err)
	}
	return nil
}

// RemoveNetworkMapCache deletes every cached network map under dir, the state
// directory of a node that is not running. Nothing else in dir is touched: the
// node's identity stays, so the device can be enabled again without enrolling.
// A directory with no cache is not an error.
func RemoveNetworkMapCache(dir string) error {
	if !filepath.IsAbs(dir) {
		return errors.New("remove cached network map: the state directory must be absolute")
	}
	profiles, err := os.ReadDir(filepath.Join(dir, profileDataDirectory))
	if errors.Is(err, os.ErrNotExist) {
		return nil
	}
	if err != nil {
		return fmt.Errorf("remove cached network map: %w", err)
	}
	var failures []error
	for _, profile := range profiles {
		if !profile.IsDir() {
			continue
		}
		cache := filepath.Join(dir, profileDataDirectory, profile.Name(), netmapCacheDirectory)
		if err := os.RemoveAll(cache); err != nil {
			failures = append(failures, fmt.Errorf("remove cached network map: %w", err))
		}
	}
	return errors.Join(failures...)
}
