//! What the gateway has done, without content: counts for `bullbar status`, kept in memory only,
//! and a decision log that is off unless asked for (concept design: logs off by default). Neither
//! ever holds a request's text, a model's answer or a credential: only outcomes, routes and the
//! engine's reasons.

use std::sync::{Arc, Mutex};

use bullbar_core::decision::{Explanation, Reason};

/// Where log lines go (standard error, in `serve`; a collector, in tests).
pub type LogSink = Arc<dyn Fn(&str) + Send + Sync>;

/// How a request ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    /// Answered on the local layer.
    Local,
    /// Answered from outside the local layer.
    Left,
    /// Refused.
    Refused,
    /// Waiting for the user's answer when the reply went.
    Asked,
    /// Stopped by the floor or the shutter.
    Blocked,
    /// Decided, but not carried out (a protection not built yet, or no address).
    NotSent,
    /// The model didn't answer.
    Failed,
}

impl Ended {
    fn name(self) -> &'static str {
        match self {
            Ended::Local => "local",
            Ended::Left => "left",
            Ended::Refused => "refused",
            Ended::Asked => "asked",
            Ended::Blocked => "blocked",
            Ended::NotSent => "not-sent",
            Ended::Failed => "failed",
        }
    }
}

/// Counts since `serve` started.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    /// Answered on the local layer.
    pub local: u64,
    /// Answered from outside.
    pub left: u64,
    /// Refused.
    pub refused: u64,
    /// Asked the user (each reply that was a question or a notice).
    pub asked: u64,
    /// Stopped by the floor or the shutter.
    pub blocked: u64,
    /// Decided but not carried out.
    pub not_sent: u64,
    /// The model didn't answer.
    pub failed: u64,
    /// Sensitive requests sent out scrubbed because there was no local runner (ADR 0014).
    pub left_for_want_of_local: u64,
    /// Requests refused because there was no local runner or model to keep them home (ADR 0014).
    pub refused_for_want_of_local: u64,
}

/// The counts and the optional decision log.
pub struct Activity {
    counts: Mutex<Counts>,
    log: Option<LogSink>,
}

impl Activity {
    /// Counting from zero; decisions logged to `log` if given.
    pub fn new(log: Option<LogSink>) -> Self {
        Activity {
            counts: Mutex::new(Counts::default()),
            log,
        }
    }

    /// Notes how a request ended: counted, and logged if logging is on.
    pub fn note(&self, ended: Ended, route: &str, explanation: &Explanation) {
        let reasons = explanation.reasons();
        let has = |wanted: fn(&Reason) -> bool| reasons.iter().any(wanted);
        {
            let mut c = self
                .counts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match ended {
                Ended::Local => c.local += 1,
                Ended::Left => c.left += 1,
                Ended::Refused => c.refused += 1,
                Ended::Asked => c.asked += 1,
                Ended::Blocked => c.blocked += 1,
                Ended::NotSent => c.not_sent += 1,
                Ended::Failed => c.failed += 1,
            }
            if ended == Ended::Left && has(|r| matches!(r, Reason::SensitiveLeavesWithoutLocal)) {
                c.left_for_want_of_local += 1;
            }
            if ended == Ended::Refused
                && has(|r| {
                    matches!(
                        r,
                        Reason::NoLocalRunnerForSensitive | Reason::NoLocalFallback
                    )
                })
            {
                c.refused_for_want_of_local += 1;
            }
        }
        if let Some(log) = &self.log {
            let why: Vec<String> = reasons.iter().map(ToString::to_string).collect();
            log(&format!(
                "decision {} route={route} why={}",
                ended.name(),
                why.join("; ")
            ));
        }
    }

    /// The counts so far.
    pub fn counts(&self) -> Counts {
        *self
            .counts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
