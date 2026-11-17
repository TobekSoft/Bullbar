//! `bullbar trace` (ADR 0018 amendment): for a while, on request, logs what left the local layer.
//! The body exactly as sent to a cloud account (scrubbed, credentials as placeholders) and the
//! provider's raw reply or error. Never the originals, the replacement map, headers (the key travels
//! in one) or anything answered locally: everything in it, the provider already has.
//!
//! Off at every start. `start(n)` covers the next n requests that leave, for an hour at most; the
//! file (`trace.jsonl` in the state folder, readable only by the user) is replaced by each start
//! and deleted by `clear`. Never standard error, so never in a container's log. An admin can forbid
//! it (`trace_allowed = false`).

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Value, json};

/// The file's name in the state folder.
pub const FILE: &str = "trace.jsonl";

/// The longest a trace runs.
pub const LONGEST: Duration = Duration::from_secs(60 * 60);

/// The most requests one trace covers.
pub const MOST: u32 = 100;

/// Why a trace couldn't be started or cleared.
#[derive(Debug, thiserror::Error)]
pub enum TraceError {
    /// The admin's policy forbids it.
    #[error("the policy forbids tracing (trace_allowed = false)")]
    Forbidden,
    /// A count outside 1 to the most.
    #[error("trace between 1 and {MOST} requests")]
    Count,
    /// The file couldn't be written or removed.
    #[error("{}: {source}", .path.display())]
    Io {
        /// Where.
        path: PathBuf,
        /// What went wrong.
        source: std::io::Error,
    },
}

/// Where a trace stands, for `status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Running {
    /// Requests it will still log.
    pub left: u32,
    /// Time before it ends anyway.
    pub ends_in: Duration,
}

struct On {
    left: u32,
    until: Instant,
    file: File,
    next: u32,
}

/// The trace switch and its file.
pub struct Trace {
    path: PathBuf,
    allowed: bool,
    on: Mutex<Option<On>>,
}

/// One request being logged: its number in the trace, and how many more the trace will log.
#[derive(Clone)]
pub struct Ticket {
    trace: Arc<Trace>,
    /// The request's number in this trace.
    pub number: u32,
    /// Requests the trace will still log after this one.
    pub left: u32,
}

impl Trace {
    /// Off, in this state folder, allowed or not by the admin's policy.
    pub fn new(state: &Path, allowed: bool) -> Self {
        Trace {
            path: state.join(FILE),
            allowed,
            on: Mutex::new(None),
        }
    }

    /// Starts logging the next `n` requests that leave, replacing the last trace's file.
    pub fn start(&self, n: u32) -> Result<(), TraceError> {
        if !self.allowed {
            return Err(TraceError::Forbidden);
        }
        if n == 0 || n > MOST {
            return Err(TraceError::Count);
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let file = options.open(&self.path).map_err(|source| TraceError::Io {
            path: self.path.clone(),
            source,
        })?;
        if let Ok(mut on) = self.on.lock() {
            *on = Some(On {
                left: n,
                until: Instant::now() + LONGEST,
                file,
                next: 1,
            });
        }
        Ok(())
    }

    /// Ends the trace; the file stays.
    pub fn stop(&self) {
        if let Ok(mut on) = self.on.lock() {
            *on = None;
        }
    }

    /// Ends the trace and deletes its file.
    pub fn clear(&self) -> Result<(), TraceError> {
        self.stop();
        match std::fs::remove_file(&self.path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(TraceError::Io {
                path: self.path.clone(),
                source: e,
            }),
            _ => Ok(()),
        }
    }

    /// Where the trace stands, if one is on and taking requests.
    pub fn running(&self) -> Option<Running> {
        let on = self.on.lock().ok()?;
        let now = Instant::now();
        on.as_ref()
            .filter(|o| o.left > 0 && o.until > now)
            .map(|o| Running {
                left: o.left,
                ends_in: o.until - now,
            })
    }

    /// Takes the next request that leaves, if a trace is on, and logs what it sends. A trace that
    /// has taken its last request keeps its file open for the answers still coming.
    pub fn take(self: &Arc<Self>, route: &str, sent: &Value) -> Option<Ticket> {
        let ticket = {
            let mut guard = self.on.lock().ok()?;
            let on = guard.as_mut()?;
            if on.left == 0 || on.until <= Instant::now() {
                return None;
            }
            on.left -= 1;
            let number = on.next;
            on.next += 1;
            Ticket {
                trace: self.clone(),
                number,
                left: on.left,
            }
        };
        ticket.write("sent", json!({ "route": route, "body": sent }));
        Some(ticket)
    }
}

impl Ticket {
    /// Logs the provider's whole reply.
    pub fn reply(&self, reply: &Value) {
        self.write("reply", reply.clone());
    }

    /// Logs one chunk of a streamed reply, as the provider sent it.
    pub fn chunk(&self, chunk: &Value) {
        self.write("chunk", chunk.clone());
    }

    /// Logs the provider's error, in full: its own message included.
    pub fn error(&self, error: &str) {
        self.write("error", Value::String(error.to_owned()));
    }

    /// The footer line for a request this trace covers.
    pub fn footer(&self) -> String {
        format!(
            "trace on: what left and the reply are logged in {FILE} ({} more)",
            self.left
        )
    }

    fn write(&self, what: &str, value: Value) {
        let seconds = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let line = json!({ "at": seconds, "request": self.number, what: value });
        if let Ok(mut on) = self.trace.on.lock()
            && let Some(on) = on.as_mut()
        {
            // A trace that can't be written protects nothing and blocks nothing: it just stops.
            if writeln!(on.file, "{line}").is_err() {
                on.left = 0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("bullbar-trace-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn lines(dir: &Path) -> Vec<Value> {
        std::fs::read_to_string(dir.join(FILE))
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn a_trace_logs_n_requests_then_stops() {
        let dir = state("n");
        let trace = Arc::new(Trace::new(&dir, true));
        assert!(trace.take("r", &json!({})).is_none(), "off at start");
        trace.start(2).unwrap();
        let first = trace.take("r", &json!({ "n": 1 })).unwrap();
        first.reply(&json!({ "answer": 1 }));
        let second = trace.take("r", &json!({ "n": 2 })).unwrap();
        assert_eq!(second.left, 0);
        second.error("HTTP 400: the provider's own words");
        assert!(trace.take("r", &json!({ "n": 3 })).is_none());
        assert!(trace.running().is_none());
        let logged = lines(&dir);
        assert_eq!(logged.len(), 4, "{logged:?}");
        assert_eq!(logged[0]["sent"]["body"]["n"], 1);
        assert_eq!(logged[1]["reply"]["answer"], 1);
        assert_eq!(logged[3]["error"], "HTTP 400: the provider's own words");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join(FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        trace.clear().unwrap();
        assert!(!dir.join(FILE).exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_forbidden_trace_never_starts() {
        let dir = state("forbidden");
        let trace = Trace::new(&dir, false);
        assert!(matches!(trace.start(5), Err(TraceError::Forbidden)));
        assert!(!dir.join(FILE).exists());
        assert!(matches!(
            Trace::new(&dir, true).start(0),
            Err(TraceError::Count)
        ));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
