//! Put Node on this process's PATH, so the things that need it can find it.
//!
//! Two subsystems shell out to the Node toolchain: the Matter controller
//! (`node`, and `npm` for its first-run install) and the stdio MCP extensions
//! (`npx`). Both look the binary up on PATH, and on a developer's machine that
//! works — an interactive shell has sourced nvm, Homebrew, fnm or whatever put
//! it there.
//!
//! A GUI-launched process has done no such thing. macOS starts an app bundle
//! from launchd with a bare `/usr/bin:/bin:/usr/sbin:/sbin`, so a Node installed
//! by nvm (`~/.nvm/versions/node/<v>/bin`) is invisible to it. The same
//! pond-server binary then behaves differently depending on whether a human
//! started it from a terminal, which is the worst kind of difference: it works
//! for whoever built it and fails for whoever installed it, with
//! "Command 'npx' not found in PATH" as the whole explanation.
//!
//! So rather than each caller resolving an absolute path — and the MCP spawn
//! cannot, since the command string lives in an extension's stored config and
//! is spawned by the MCP client — this runs once at startup and fixes the
//! environment every child inherits.

use std::path::PathBuf;

/// Directories a Node install is plausibly in, most specific first.
///
/// nvm and fnm keep a directory per version, so those are globbed rather than
/// listed. Everything else is a fixed location.
fn candidate_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();

    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        // nvm's `default` alias is not a directory, so the versions are read
        // and sorted; the highest version wins, which is what `nvm use default`
        // usually means in practice.
        for base in [".nvm/versions/node", ".local/share/fnm/node-versions"] {
            let root = home.join(base);
            if let Ok(entries) = std::fs::read_dir(&root) {
                let mut versions: Vec<PathBuf> = entries
                    .filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.is_dir())
                    .collect();
                // Lexicographic, which orders v20 < v22 < v24 correctly for the
                // versions that exist; a wrong pick here still yields a working
                // Node, and the Matter floor check rejects one that is too old.
                versions.sort();
                dirs.extend(
                    versions
                        .into_iter()
                        .rev()
                        .flat_map(|v| [v.join("bin"), v.join("installation/bin")].into_iter()),
                );
            }
        }
        dirs.push(home.join(".volta/bin"));
        dirs.push(home.join(".asdf/shims"));
        dirs.push(home.join(".local/bin"));
    }

    // Homebrew (Apple silicon, then Intel), then the system locations a distro
    // package would use.
    dirs.push(PathBuf::from("/opt/homebrew/bin"));
    dirs.push(PathBuf::from("/usr/local/bin"));
    dirs.push(PathBuf::from("/usr/bin"));

    dirs
}

/// Is `dir` a directory holding a runnable `node`?
fn holds_node(dir: &std::path::Path) -> bool {
    dir.join("node").is_file()
}

/// The directory Node is already reachable from within `path`, if any.
fn node_dir_in(path: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    std::env::split_paths(path?).find(|dir| holds_node(dir))
}

/// The directory to prepend so Node is reachable, given `path`, or `None` when
/// it already is (or nothing can be found).
///
/// Split out from the environment so the decision is testable: the bug this
/// module exists for only appears under a PATH the test process does not have,
/// and a check that can only run under the developer's own PATH would pass on
/// the one machine where the bug never happens.
fn dir_to_prepend(path: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    if node_dir_in(path).is_some() {
        return None;
    }
    candidate_dirs().into_iter().find(|dir| holds_node(dir))
}

/// Prepend a Node directory to `PATH` when Node is not already reachable.
///
/// Returns the directory added, or `None` when nothing needed doing (Node was
/// already there) or nothing could be found.
///
/// Calls [`std::env::set_var`], which is only sound while no other thread is
/// reading the environment. Call it once, at the top of `main`, before any
/// child is spawned — which is also the only point at which it would do any
/// good.
pub fn ensure_node_on_path() -> Option<PathBuf> {
    let current = std::env::var_os("PATH").unwrap_or_default();
    let Some(found) = dir_to_prepend(Some(&current)) else {
        tracing::debug!("node: already reachable, or nowhere to be found");
        return None;
    };

    let mut dirs = vec![found.clone()];
    dirs.extend(std::env::split_paths(&current));
    let joined = std::env::join_paths(dirs).ok()?;
    std::env::set_var("PATH", joined);

    tracing::info!(
        target: "giap::trace",
        kind = "node_path_extended",
        dir = %found.display(),
        "node was not on PATH; added the directory it is in so Matter and the \
         stdio extensions can find it"
    );
    Some(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_directory_without_node_is_not_offered() {
        let empty = std::env::temp_dir().join(format!("giap-nodepath-{}", std::process::id()));
        std::fs::create_dir_all(&empty).unwrap();
        assert!(!holds_node(&empty));

        std::fs::write(empty.join("node"), "#!/bin/sh\n").unwrap();
        assert!(
            holds_node(&empty),
            "a directory with a node in it qualifies"
        );

        std::fs::remove_dir_all(&empty).unwrap();
    }

    #[test]
    fn the_candidate_list_covers_the_managers_people_actually_use() {
        // A list that quietly lost an entry would reintroduce exactly the bug
        // this module exists for, on whichever machine used that manager.
        let dirs = candidate_dirs();
        let rendered: Vec<String> = dirs.iter().map(|d| d.display().to_string()).collect();
        let joined = rendered.join(" ");

        for expected in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"] {
            assert!(joined.contains(expected), "missing {expected} in {joined}");
        }
        if std::env::var_os("HOME").is_some() {
            for expected in [".volta/bin", ".asdf/shims"] {
                assert!(joined.contains(expected), "missing {expected}");
            }
        }
    }

    /// The actual bug, reproduced: launchd's PATH, which is what a GUI-launched
    /// pond-server gets, and under which nvm's node is invisible.
    #[test]
    fn a_launchd_path_gets_node_prepended() {
        let bare = std::ffi::OsString::from("/usr/bin:/bin:/usr/sbin:/sbin");

        // Only meaningful on a machine whose Node is NOT in a system location —
        // which is the machine the bug happens on. Elsewhere there is nothing
        // to fix and nothing to assert.
        let Some(found) = dir_to_prepend(Some(&bare)) else {
            return;
        };
        assert!(
            holds_node(&found),
            "offered {} as a node directory",
            found.display()
        );
        assert!(
            !bare
                .to_string_lossy()
                .contains(&found.display().to_string()),
            "prepended a directory that was already on the PATH"
        );
    }

    /// A PATH that already reaches Node is left alone, so the ordinary
    /// terminal-launched case is untouched.
    #[test]
    fn a_path_that_already_reaches_node_is_left_alone() {
        let Some(dir) = node_dir_in(std::env::var_os("PATH").as_deref()) else {
            return; // no Node here
        };
        let just_that = std::ffi::OsString::from(dir.display().to_string());
        assert!(dir_to_prepend(Some(&just_that)).is_none());
    }
}
