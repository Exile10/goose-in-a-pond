//! One voice session per device.
//!
//! A voice session owns two pieces of hardware there is only one of: the
//! microphone and the speaker. Two sessions do not degrade gracefully — they
//! fight. The symptoms are indirect and hard to attribute:
//!
//! - both answer, in different voices, because each resolved its own settings
//! - one grabs the input device with a sample format the other cannot reopen,
//!   and the loser dies with "The requested stream configuration is not
//!   supported by the device"
//! - the wake word fires in one process for speech meant for the other
//!
//! None of those name the real problem, which is simply that a second session
//! exists. So the second session refuses to start and says so.
//!
//! ## Why the lock is not in the data directory
//!
//! The contended resource is the *device*, not the database. A session started
//! with `POND_DATA_DIR` pointed elsewhere — a test, a scratch profile — takes
//! the same microphone as the ordinary one, so it must take the same lock.
//! Scoping the lock to the data directory would have let exactly that pair run
//! side by side, which is the case this module exists to prevent.
//!
//! Per user, not per machine: two accounts on one desktop have separate audio
//! sessions, and one should not lock out the other.
//!
//! ## Why `flock` rather than a pidfile
//!
//! The kernel drops the lock when the holding process dies, however it dies.
//! A pidfile has to be reconciled with reality on every startup — is that PID
//! alive, is it still ours, did it get recycled — and a stale one either
//! blocks every future launch or gets ignored so eagerly it stops being a
//! lock. There is no stale `flock`.
//!
//! The PID is still written *into* the file, purely so the refusal message can
//! name the process holding it. It is a diagnostic, never a decision.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

use anyhow::{anyhow, Result};
use fs2::FileExt;

/// Held for the lifetime of a voice session; releases on drop.
pub struct VoiceLock {
    file: File,
    path: PathBuf,
}

/// Path of the per-user, device-wide voice lock.
fn lock_path() -> PathBuf {
    // The user id keeps two accounts on one machine independent. On Windows
    // there is no uid; the per-user temp directory already provides the
    // separation, so a fixed name is enough.
    #[cfg(unix)]
    let name = format!("giap-voice-{}.lock", unsafe { libc::getuid() });
    #[cfg(not(unix))]
    let name = "giap-voice.lock".to_string();

    std::env::temp_dir().join(name)
}

impl VoiceLock {
    /// Take the device's voice lock, or explain who already has it.
    ///
    /// The error is written to be read by whoever hit it, not by whoever wrote
    /// it: it says what is wrong, which process is responsible, and what to do.
    pub fn acquire() -> Result<Self> {
        let path = lock_path();
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| anyhow!("cannot open the voice lock at {}: {e}", path.display()))?;

        if file.try_lock_exclusive().is_err() {
            // Read the holder's PID for the message. Best effort — a holder
            // mid-write, or an older build that wrote nothing, must not turn a
            // clear refusal into a confusing one.
            let mut holder = String::new();
            let _ = file.read_to_string(&mut holder);
            let holder = holder.trim();

            let who = if holder.is_empty() {
                "another voice session".to_string()
            } else {
                format!("another voice session (pid {holder})")
            };

            return Err(anyhow!(
                "{who} is already running on this device.\n\
                 \n\
                 Voice needs sole use of the microphone and speaker. Two sessions \
                 answer in different voices and take the audio device from each \
                 other.\n\
                 \n\
                 Stop the other session first{}.",
                if holder.is_empty() {
                    String::new()
                } else {
                    format!(" — `kill {holder}`")
                }
            ));
        }

        // Record who holds it, for the next process's refusal message.
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        writeln!(file, "{}", std::process::id())?;
        file.flush()?;

        tracing::debug!("voice lock acquired at {}", path.display());
        Ok(Self { file, path })
    }
}

impl Drop for VoiceLock {
    fn drop(&mut self) {
        // Best effort. The kernel releases the lock when this process exits
        // whatever happens here, which is the guarantee the design rests on;
        // unlinking just keeps the temp directory tidy.
        let _ = FileExt::unlock(&self.file);
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lock is process-wide state, so these would race each other under
    /// cargo's parallel harness and each would "pass" by taking the
    /// bail-out branch — testing nothing. One at a time.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        SERIAL.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The lock must not depend on `POND_DATA_DIR`. A scratch-profile session
    /// takes the same microphone as the real one — that pair running side by
    /// side is exactly the failure this prevents.
    #[test]
    fn the_lock_is_independent_of_the_data_directory() {
        let before = lock_path();
        std::env::set_var("POND_DATA_DIR", "/tmp/some-other-profile");
        let after = lock_path();
        std::env::remove_var("POND_DATA_DIR");
        assert_eq!(
            before, after,
            "the device is the resource, not the database"
        );
    }

    /// Two accounts on one machine have separate audio sessions.
    #[cfg(unix)]
    #[test]
    fn the_lock_is_scoped_to_the_user() {
        let name = lock_path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        let uid = unsafe { libc::getuid() };
        assert!(name.contains(&uid.to_string()), "{name} is not user-scoped");
    }

    /// The whole point: the second caller is refused, and told who to blame.
    #[test]
    fn a_second_session_is_refused_and_names_the_holder() {
        let _serial = serial();
        let first = VoiceLock::acquire().expect("nothing else should hold the lock in a test run");

        let err = VoiceLock::acquire()
            .err()
            .expect("a second session must be refused")
            .to_string();

        assert!(
            err.contains(&std::process::id().to_string()),
            "the refusal must name the holding process: {err}"
        );
        assert!(
            err.contains("microphone"),
            "the refusal must say why, not just no: {err}"
        );

        drop(first);
    }

    /// Releasing must actually release, or a clean exit would lock the user
    /// out of their own assistant until reboot.
    #[test]
    fn releasing_lets_the_next_session_start() {
        let _serial = serial();
        let first = VoiceLock::acquire().expect("nothing else should hold the lock in a test run");
        drop(first);

        let second = VoiceLock::acquire().expect("the lock must be free once the holder has gone");
        drop(second);
    }
}
