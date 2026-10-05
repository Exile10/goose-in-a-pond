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

// ForgetNetworkMapWhenRefused erases the node's cached network map whenever the
// coordinator answers it with a login URL, and returns when ctx is done.
//
// A login URL means the coordinator no longer accepts this node: it was removed,
// or its key expired. The backend keeps the cached map in that case, stays
// Running on it, and loads it at every start, so a removed phone would otherwise
// carry its Pond's addresses and the access rules until it next reached the Pond
// on the home network, the only place it learns it was forgotten. Nothing is lost
// by erasing it: the node cannot use the map until it enrolls again, and
// enrolling writes a new one.
//
// The backend announces a login URL once, so a refusal that arrived before the
// watch began is read from the node's status instead. A node with nothing
// cached, one enrolling for the first time, is left alone. report receives one
// line per erasure and per failure; ctx ending is not one.
func (n *Node) ForgetNetworkMapWhenRefused(ctx context.Context, report func(string)) {
	client, err := n.Server.LocalClient()
	if err != nil {
		report("cannot watch for the coordinator refusing this device: " + err.Error())
		return
	}
	watcher, err := client.WatchIPNBus(ctx, 0)
	if err != nil {
		if ctx.Err() == nil {
			report("cannot watch for the coordinator refusing this device: " + err.Error())
		}
		return
	}
	defer watcher.Close()
	// Read after the watch is open, so a login URL is either here or in a notice.
	status, err := client.StatusWithoutPeers(ctx)
	if err != nil {
		if ctx.Err() == nil {
			report("cannot read whether the coordinator refused this device: " + err.Error())
		}
	} else if status.AuthURL != "" {
		n.forgetRefusedNetworkMap(ctx, report)
	}
	for {
		notify, err := watcher.Next()
		if err != nil {
			if ctx.Err() == nil {
				report("stopped watching for the coordinator refusing this device: " + err.Error())
			}
			return
		}
		if notify.BrowseToURL != nil && *notify.BrowseToURL != "" {
			n.forgetRefusedNetworkMap(ctx, report)
		}
	}
}

// forgetRefusedNetworkMap is one erasure for ForgetNetworkMapWhenRefused.
func (n *Node) forgetRefusedNetworkMap(ctx context.Context, report func(string)) {
	cached, err := hasNetworkMapCache(n.Server.Dir)
	if err != nil {
		report("the coordinator refused this device, and its cached network map could not be checked: " + err.Error())
		return
	}
	if !cached {
		return
	}
	// The backend's clear drops the copy it holds in memory, but it does not
	// report a failure to delete, so the files are removed and checked here too.
	// No map can arrive to be rewritten in between: the coordinator sends none to
	// a node it refuses.
	if err := n.ClearNetworkMapCache(); err != nil && ctx.Err() == nil {
		report("the running node did not clear its cached network map; erasing it from disk instead: " + err.Error())
	}
	if err := RemoveNetworkMapCache(n.Server.Dir); err != nil {
		report("the coordinator refused this device, but its cached network map was not erased: " + err.Error())
		return
	}
	report("the coordinator no longer accepts this device, so its cached network map was erased")
}

// hasNetworkMapCache reports whether any profile under dir holds a cached map.
func hasNetworkMapCache(dir string) (bool, error) {
	files, err := filepath.Glob(filepath.Join(dir, profileDataDirectory, "*", netmapCacheDirectory, "*"))
	return len(files) > 0, err
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
