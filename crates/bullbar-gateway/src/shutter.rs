//! The kill switch: the first manual shutter, level 3 in the concept design ("local only"). Closed,
//! nothing leaves the local layer; local answers go on. Closing is one command; reopening needs a
//! person, through the control channel or at this machine.
//!
//! The state lives in a file in the state folder, so a restart doesn't quietly reopen. A file that
//! can't be read counts as closed (rule 4).

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use bullbar_core::provider::Date;

/// The file in the state folder that holds a closed shutter.
pub const SHUTTER_FILE: &str = "shutter";

/// A closed shutter: since when, and why. No request content, ever.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Closed {
    /// Seconds since 1970, UTC.
    pub since: u64,
    /// Why, as given when closing (or why the state couldn't be read).
    pub reason: String,
}

/// Why the shutter couldn't be changed. The in-memory state is left as it was.
#[derive(Debug, thiserror::Error)]
#[error("{}: the shutter state can't be written: {source}", .path.display())]
pub struct ShutterError {
    /// The state file.
    pub path: PathBuf,
    /// What went wrong.
    #[source]
    pub source: std::io::Error,
}

/// Reads the shutter state from the state folder: no file is open; a file is closed, and so is one
/// that can't be read or understood.
pub fn read(state: &Path) -> Option<Closed> {
    let path = state.join(SHUTTER_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(_) => return Some(unreadable()),
    };
    let field = |name: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(name)?.strip_prefix('='))
            .map(str::to_owned)
    };
    match (
        text.lines().next(),
        field("since").and_then(|s| s.parse().ok()),
    ) {
        (Some("closed"), Some(since)) => Some(Closed {
            since,
            reason: field("reason").unwrap_or_default(),
        }),
        _ => Some(unreadable()),
    }
}

fn unreadable() -> Closed {
    Closed {
        since: now(),
        reason: "the shutter file couldn't be read, so it counts as closed".to_owned(),
    }
}

/// Closes the shutter in the state folder.
pub fn close(state: &Path, reason: &str) -> Result<Closed, ShutterError> {
    let closed = Closed {
        since: now(),
        // One line: the file is line-based.
        reason: reason.replace(['\n', '\r'], " "),
    };
    let path = state.join(SHUTTER_FILE);
    let err = |source| ShutterError {
        path: path.clone(),
        source,
    };
    std::fs::create_dir_all(state).map_err(err)?;
    // Written whole, then renamed into place: a crash never leaves half a file.
    let partial = state.join(format!("{SHUTTER_FILE}.partial"));
    std::fs::write(
        &partial,
        format!("closed\nsince={}\nreason={}\n", closed.since, closed.reason),
    )
    .map_err(err)?;
    std::fs::rename(&partial, &path).map_err(err)?;
    Ok(closed)
}

/// Opens the shutter in the state folder.
pub fn open(state: &Path) -> Result<(), ShutterError> {
    let path = state.join(SHUTTER_FILE);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(ShutterError { path, source }),
    }
}

/// The running gateway's shutter: the file's state, kept in memory and changed through it.
pub struct Shutter {
    state: PathBuf,
    current: Mutex<Option<Closed>>,
}

impl Shutter {
    /// Loads the state from the folder.
    pub fn load(state: &Path) -> Self {
        Shutter {
            state: state.to_owned(),
            current: Mutex::new(read(state)),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Option<Closed>> {
        // A poisoned lock still holds a whole value; carry on with it.
        self.current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Closed, and since when.
    pub fn closed(&self) -> Option<Closed> {
        self.lock().clone()
    }

    /// Closes it: memory first, so nothing leaves from now on, then the file. If the file can't
    /// be written it stays closed for this run, and the error says a restart would reopen it.
    pub fn close(&self, reason: &str) -> Result<Closed, ShutterError> {
        let mut current = self.lock();
        *current = Some(Closed {
            since: now(),
            reason: reason.to_owned(),
        });
        let closed = close(&self.state, reason)?;
        *current = Some(closed.clone());
        Ok(closed)
    }

    /// Reopens it: the file first, then memory, so a failure leaves it closed.
    pub fn open(&self) -> Result<(), ShutterError> {
        let mut current = self.lock();
        open(&self.state)?;
        *current = None;
        Ok(())
    }
}

/// Seconds since 1970 as a UTC date and time, to the minute: `2026-11-16 09:30 UTC`.
pub fn utc(secs: u64) -> String {
    let date = i64::try_from(secs / 86_400)
        .ok()
        .and_then(Date::from_days_since_1970)
        .map_or_else(|| "?".to_owned(), |d| d.to_string());
    let minutes = secs % 86_400 / 60;
    format!("{date} {:02}:{:02} UTC", minutes / 60, minutes % 60)
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "bullbar-shutter-test-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_closed_shutter_survives_a_restart() {
        let dir = state("restart");
        assert_eq!(read(&dir), None);
        let shutter = Shutter::load(&dir);
        let closed = shutter.close("testing the switch").unwrap();
        assert_eq!(Shutter::load(&dir).closed(), Some(closed));
        shutter.open().unwrap();
        assert_eq!(Shutter::load(&dir).closed(), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_file_that_cannot_be_understood_counts_as_closed() {
        let dir = state("garbled");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(SHUTTER_FILE), "open please\n").unwrap();
        let closed = read(&dir).unwrap();
        assert!(closed.reason.contains("counts as closed"), "{closed:?}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn times_read_as_utc() {
        assert_eq!(utc(0), "1970-01-01 00:00 UTC");
        assert_eq!(utc(1_794_783_899), "2026-11-15 23:04 UTC");
    }

    #[test]
    fn a_reason_stays_on_one_line() {
        let dir = state("oneline");
        close(&dir, "first\nsince=0").unwrap();
        let closed = read(&dir).unwrap();
        assert_eq!(closed.reason, "first since=0");
        assert_ne!(closed.since, 0);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
