//! Questions waiting for the user (ADR 0016). A request whose decision is a question is parked
//! here, in memory, with the conversation it belongs to. The answer comes only through the control
//! channel; the chat client sees a hold, then a notice, and resends the conversation to continue.
//!
//! No answer counts as no: a question left past its expiry is settled as refused, and a restart
//! loses every question, so the request is asked again from the start. Neither ever loosens.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use bullbar_core::decision::Question;
use bullbar_core::lifecycle::{Event, Lifecycle, State};
use tokio::sync::Notify;

/// The user's answer, or its absence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// Yes to the question.
    Yes,
    /// No to the question.
    No,
    /// Not answered before it expired: settled as no.
    Expired,
}

impl Answer {
    /// The lifecycle event for this answer to this question.
    pub fn event(self, question: &Question) -> Event {
        match (self, question) {
            (Answer::Yes, Question::ConfirmLowerRating { .. }) => Event::UserConfirmsLowerRating,
            (Answer::Yes, Question::SettleLocal { .. }) => Event::UserSettles,
            (Answer::Yes, Question::ApproveEscalation { .. }) => Event::UserApprovesEscalation,
            (Answer::Yes, Question::MovePin { .. }) => Event::UserMovesPin,
            (Answer::No | Answer::Expired, _) => Event::UserRefuses,
        }
    }
}

/// Why an answer wasn't taken.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AnswerError {
    /// No such question (never asked, already settled, or lost in a restart).
    #[error("no open question `{0}`")]
    Unknown(String),
    /// Already answered, or expired.
    #[error("question `{0}` is already settled")]
    Settled(String),
}

/// A question as the control channel lists it: facts, not the conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    /// Its id, for `bullbar answer`.
    pub id: String,
    /// The model name the request asked for.
    pub model: String,
    /// The question.
    pub question: String,
    /// The reasons behind it, in order.
    pub why: Vec<String>,
    /// How long it has waited.
    pub waiting: Duration,
    /// The answer, if settled.
    pub answer: Option<Answer>,
}

struct Entry {
    /// Which conversation it belongs to (see `server::conversation_key`).
    key: u64,
    model: String,
    lifecycle: Lifecycle,
    asked: Instant,
    answer: Option<Answer>,
    answered: Option<Instant>,
    notify: Arc<Notify>,
}

impl Entry {
    fn question(&self) -> Option<&Question> {
        match self.lifecycle.state() {
            State::AwaitingUser(question) => Some(question),
            _ => None,
        }
    }
}

/// The open questions.
pub struct Pending {
    entries: Mutex<BTreeMap<String, Entry>>,
    expiry: Duration,
}

/// A parked question to wait on.
pub struct Waiting {
    /// Its id.
    pub id: String,
    /// Woken when it is answered.
    pub notify: Arc<Notify>,
}

impl Pending {
    /// An empty store; unanswered questions expire after `expiry`.
    pub fn new(expiry: Duration) -> Self {
        Pending {
            entries: Mutex::new(BTreeMap::new()),
            expiry,
        }
    }

    /// Locks the store, settling what has expired. A poisoned lock means another request panicked
    /// while holding it; the entries are still consistent (each change is one insert or field
    /// write), so carry on rather than take every question down.
    fn lock(&self) -> MutexGuard<'_, BTreeMap<String, Entry>> {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        for entry in entries.values_mut() {
            if entry.answer.is_none() && now.duration_since(entry.asked) >= self.expiry {
                entry.answer = Some(Answer::Expired);
                entry.answered = Some(now);
                entry.notify.notify_waiters();
            }
        }
        // A settled question nobody came back for is dropped after a while (at least ten minutes,
        // so a "no" is still there to find when the user resends); coming back later asks again,
        // which never loosens.
        let keep = (self.expiry * 4).max(Duration::from_secs(600));
        entries.retain(|_, e| e.answered.is_none_or(|t| now.duration_since(t) < keep));
        entries
    }

    /// Parks a request waiting for an answer. `id` must be fresh.
    pub fn park(&self, id: String, key: u64, model: &str, lifecycle: Lifecycle) -> Waiting {
        let notify = Arc::new(Notify::new());
        self.lock().insert(
            id.clone(),
            Entry {
                key,
                model: model.to_owned(),
                lifecycle,
                asked: Instant::now(),
                answer: None,
                answered: None,
                notify: notify.clone(),
            },
        );
        Waiting { id, notify }
    }

    /// The unanswered question this conversation is already waiting on, if any.
    pub fn waiting_for(&self, key: u64) -> Option<Waiting> {
        self.lock()
            .iter()
            .find(|(_, e)| e.key == key && e.answer.is_none())
            .map(|(id, e)| Waiting {
                id: id.clone(),
                notify: e.notify.clone(),
            })
    }

    /// Takes a settled question for this conversation: its lifecycle, still awaiting the user, and
    /// the answer to feed it. Taken once; the conversation's next request starts fresh.
    pub fn take_settled(&self, key: u64) -> Option<(String, Lifecycle, Answer)> {
        let mut entries = self.lock();
        let id = entries
            .iter()
            .find(|(_, e)| e.key == key && e.answer.is_some())
            .map(|(id, _)| id.clone())?;
        let entry = entries.remove(&id)?;
        Some((id, entry.lifecycle, entry.answer?))
    }

    /// Answers a question (the control channel only).
    pub fn answer(&self, id: &str, answer: Answer) -> Result<(), AnswerError> {
        let mut entries = self.lock();
        let entry = entries
            .get_mut(id)
            .ok_or_else(|| AnswerError::Unknown(id.to_owned()))?;
        if entry.answer.is_some() {
            return Err(AnswerError::Settled(id.to_owned()));
        }
        entry.answer = Some(answer);
        entry.answered = Some(Instant::now());
        entry.notify.notify_waiters();
        Ok(())
    }

    /// Every question, oldest first.
    pub fn list(&self) -> Vec<Listed> {
        let entries = self.lock();
        let mut listed: Vec<Listed> = entries
            .iter()
            .map(|(id, e)| Listed {
                id: id.clone(),
                model: e.model.clone(),
                question: e.question().map(ToString::to_string).unwrap_or_default(),
                why: e
                    .lifecycle
                    .explanation()
                    .reasons()
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
                waiting: e.asked.elapsed(),
                answer: e.answer,
            })
            .collect();
        listed.sort_by_key(|item| std::cmp::Reverse(item.waiting));
        listed
    }
}

/// A short id for a question, easy to type: six characters from an alphabet without look-alikes.
pub fn new_id() -> Result<String, getrandom::Error> {
    const ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
    let mut bytes = [0u8; 6];
    getrandom::fill(&mut bytes)?;
    Ok(bytes
        .iter()
        .map(|b| char::from(ALPHABET[usize::from(*b) % ALPHABET.len()]))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bullbar_core::engine::Engine;
    use bullbar_core::levels::{Category, Column, Complexity, Row, Sensitivity};
    use bullbar_core::policy::{AdminPolicy, Policy};
    use bullbar_core::profile::RequestProfile;
    use bullbar_core::provider::{Offer, ProviderAccount};
    use bullbar_core::slots::{Slot, SlotState, Slots};

    /// Hard personal work with only a mid local model: the engine asks whether to settle.
    fn asking() -> (Engine, Lifecycle) {
        let local = ProviderAccount::new(
            "local".parse().unwrap(),
            Row::Local,
            vec![Offer {
                column: Column::Mid,
                cost: 0,
            }],
        );
        let engine = Engine::new(
            Policy::from_admin(AdminPolicy::default()),
            vec![local],
            Slots::required_only().with(Slot::LocalRunner, SlotState::Healthy),
        )
        .unwrap();
        let profile = RequestProfile {
            user_rating: Some(Sensitivity::Personal),
            ..RequestProfile::new(Complexity::Hard, Category::General)
        };
        let lifecycle = Lifecycle::start(&engine, profile, "2026-11-16".parse().unwrap());
        assert!(matches!(lifecycle.state(), State::AwaitingUser(_)));
        (engine, lifecycle)
    }

    #[test]
    fn an_answer_is_taken_once_by_its_conversation() {
        let (_, lifecycle) = asking();
        let pending = Pending::new(Duration::from_secs(60));
        let waiting = pending.park("abc".into(), 7, "bullbar", lifecycle);
        assert_eq!(pending.waiting_for(7).map(|w| w.id), Some("abc".into()));
        assert!(pending.take_settled(7).is_none());
        pending.answer(&waiting.id, Answer::Yes).unwrap();
        assert_eq!(
            pending.answer("abc", Answer::No),
            Err(AnswerError::Settled("abc".into()))
        );
        assert!(pending.take_settled(8).is_none());
        let (_, _, answer) = pending.take_settled(7).unwrap();
        assert_eq!(answer, Answer::Yes);
        assert!(pending.take_settled(7).is_none());
    }

    #[test]
    fn no_answer_counts_as_no() {
        let (engine, lifecycle) = asking();
        let pending = Pending::new(Duration::ZERO);
        pending.park("abc".into(), 7, "bullbar", lifecycle);
        assert_eq!(pending.list()[0].answer, Some(Answer::Expired));
        let (_, mut lifecycle, answer) = pending.take_settled(7).unwrap();
        let State::AwaitingUser(question) = lifecycle.state().clone() else {
            panic!("not awaiting");
        };
        lifecycle.handle(&engine, answer.event(&question)).unwrap();
        assert_eq!(lifecycle.state(), &State::Refused);
    }

    #[test]
    fn an_unknown_question_cannot_be_answered() {
        let pending = Pending::new(Duration::from_secs(60));
        assert_eq!(
            pending.answer("nope", Answer::Yes),
            Err(AnswerError::Unknown("nope".into()))
        );
    }

    #[test]
    fn ids_are_short_and_typeable() {
        let id = new_id().unwrap();
        assert_eq!(id.len(), 6);
        assert!(
            id.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        );
    }
}
