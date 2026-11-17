//! The request lifecycle (ADR 0013): what happens between the first decision and a final answer.
//!
//! A pure state machine. The gateway (chunk 2) sends each route and feeds back the answer's
//! verdict and the user's choices as events; the lifecycle asks the engine for each next attempt.
//! Two moves after a poor answer, never mixed:
//! - **escalate**: a more capable column, inside the same ceiling (the answer check or the user);
//! - **one rung further**: the ceiling up one row, still scrubbed (only the user, ADR 0011).
//!
//! Every attempt lands on a new cell, strictly forward on (column, then row), so the loop ends.

use std::fmt;

use crate::decision::{Explanation, Outcome, Question, Route};
use crate::engine::Engine;
use crate::levels::{Column, Row};
use crate::policy::Escalation;
use crate::profile::RequestProfile;
use crate::provider::Date;
use crate::slots::Slot;

/// What has happened to a request so far that changes the next decision. The profile describes
/// the request; the attempt describes the conversation about it (ADR 0013 §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Attempt {
    /// The user has confirmed a rating below the classifier's.
    pub lower_rating_confirmed: bool,
    /// One-off exception rungs taken (ADR 0011).
    pub rungs: u8,
    /// Only models at least this capable: set when escalating after a poor answer.
    pub column_floor: Option<Column>,
    /// Only rows at least this exposed: set when retrying one rung further.
    pub row_floor: Option<Row>,
}

/// Who judged an answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Judge {
    /// The answer check slot: the model's confidence, a local judge model, tests for code.
    Screener,
    /// The user.
    User,
}

/// Something that happened, fed in by the gateway.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// The answer came back and was judged.
    Answered {
        /// Who judged it.
        judged_by: Judge,
        /// Good enough.
        good: bool,
    },
    /// Yes to a rating below the classifier's.
    UserConfirmsLowerRating,
    /// Yes to the best local model, labelled as such.
    UserSettles,
    /// Yes to an escalation the answer check proposed.
    UserApprovesEscalation,
    /// Yes to moving the request's topic to another account (ADR 0021).
    UserMovesPin,
    /// No to the question asked. For a lower rating: keep the classifier's rating and go on.
    UserRefuses,
    /// After a poor answer: try a more capable model.
    UserAsksMoreCapable,
    /// After a poor answer: one rung further up the exposure ladder.
    UserAsksOneRung,
    /// After a poor answer: keep it.
    UserAccepts,
    /// Stop here.
    UserStops,
}

/// Where a request is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// Waiting for the user to answer a question.
    AwaitingUser(Question),
    /// Sent along this route; waiting for the answer.
    Sent(Route),
    /// The answer from this route was judged poor; waiting for the user's call.
    Poor(Route),
    /// Finished with the answer from this route.
    Done(Route),
    /// Not sent anywhere.
    Refused,
    /// The user stopped it.
    Stopped,
}

impl State {
    /// A short name, for errors.
    pub fn name(&self) -> &'static str {
        match self {
            State::AwaitingUser(_) => "waiting for the user",
            State::Sent(_) => "sent",
            State::Poor(_) => "judged poor",
            State::Done(_) => "done",
            State::Refused => "refused",
            State::Stopped => "stopped",
        }
    }
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            State::AwaitingUser(question) => write!(f, "ask: {question}"),
            State::Sent(route) => write!(f, "sent: {route}"),
            State::Poor(route) => write!(f, "judged poor: {route}"),
            State::Done(route) => write!(f, "done: {route}"),
            State::Refused => f.write_str("refused"),
            State::Stopped => f.write_str("stopped"),
        }
    }
}

/// An event the lifecycle can't take. Nothing changes.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LifecycleError {
    /// The event doesn't fit the state.
    #[error("{event:?} doesn't fit a request that is {state}")]
    NotNow {
        /// The event.
        event: Event,
        /// The state's name.
        state: &'static str,
    },
    /// A screener verdict with no working answer check: only the user's verdict counts.
    #[error("a screener verdict, but no working answer check")]
    NoAnswerCheck,
}

/// One request from its first decision to its end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lifecycle {
    profile: RequestProfile,
    today: Date,
    attempt: Attempt,
    /// The attempt behind an escalation waiting for the user's approval.
    pending: Option<Attempt>,
    state: State,
    sent: Vec<Route>,
    explanation: Explanation,
}

impl Lifecycle {
    /// Decides the first attempt at a request.
    pub fn start(engine: &Engine, profile: RequestProfile, today: Date) -> Self {
        let mut lifecycle = Lifecycle {
            profile,
            today,
            attempt: Attempt::default(),
            pending: None,
            state: State::Refused,
            sent: Vec::new(),
            explanation: Explanation::default(),
        };
        lifecycle.enter(engine, Attempt::default());
        lifecycle
    }

    /// Where the request is.
    pub fn state(&self) -> &State {
        &self.state
    }

    /// The request as it now stands. Declining a lower rating clears the user's rating.
    pub fn profile(&self) -> &RequestProfile {
        &self.profile
    }

    /// What the current attempt carries.
    pub fn attempt(&self) -> &Attempt {
        &self.attempt
    }

    /// Every route the request was sent along, in order.
    pub fn sent(&self) -> &[Route] {
        &self.sent
    }

    /// The reasons behind the latest decision, including one that found nothing to try next.
    pub fn explanation(&self) -> &Explanation {
        &self.explanation
    }

    /// Takes an event. One that doesn't fit the state is an error and changes nothing.
    pub fn handle(&mut self, engine: &Engine, event: Event) -> Result<(), LifecycleError> {
        if let Event::Answered {
            judged_by: Judge::Screener,
            ..
        } = event
            && !engine.slot_usable(Slot::AnswerCheck)
        {
            return Err(LifecycleError::NoAnswerCheck);
        }
        let next = match (self.state.clone(), event) {
            (
                State::AwaitingUser(Question::ConfirmLowerRating { .. }),
                Event::UserConfirmsLowerRating,
            ) => {
                let attempt = Attempt {
                    lower_rating_confirmed: true,
                    ..self.attempt
                };
                self.enter(engine, attempt);
                return Ok(());
            }
            (State::AwaitingUser(Question::ConfirmLowerRating { .. }), Event::UserRefuses) => {
                self.profile.user_rating = None;
                self.enter(engine, self.attempt);
                return Ok(());
            }
            (State::AwaitingUser(Question::SettleLocal { route, .. }), Event::UserSettles) => {
                self.send(route)
            }
            (State::AwaitingUser(Question::SettleLocal { .. }), Event::UserRefuses) => {
                State::Refused
            }
            // The profile keeps the pin the decision saw: whoever records the move checks the topic
            // is still pinned there. A retry of this request asks again rather than follow.
            (State::AwaitingUser(Question::MovePin { route, .. }), Event::UserMovesPin) => {
                self.send(route)
            }
            (State::AwaitingUser(Question::MovePin { .. }), Event::UserRefuses) => State::Refused,
            (
                State::AwaitingUser(Question::ApproveEscalation { route }),
                Event::UserApprovesEscalation,
            ) => {
                if let Some(attempt) = self.pending.take() {
                    self.attempt = attempt;
                }
                self.send(route)
            }
            (State::AwaitingUser(Question::ApproveEscalation { .. }), Event::UserRefuses) => {
                self.pending = None;
                match self.sent.last() {
                    Some(last) => State::Poor(last.clone()),
                    None => State::Refused,
                }
            }
            (State::Sent(route), Event::Answered { good: true, .. }) => State::Done(route),
            (
                State::Sent(route),
                Event::Answered {
                    judged_by: Judge::User,
                    good: false,
                },
            ) => State::Poor(route),
            (
                State::Sent(route),
                Event::Answered {
                    judged_by: Judge::Screener,
                    good: false,
                },
            ) => self.screener_escalates(engine, route),
            (State::Poor(route), Event::UserAccepts) => State::Done(route),
            (State::Poor(route), Event::UserAsksMoreCapable) => {
                match self.try_next(engine, self.more_capable(&route)) {
                    Some((next, attempt)) => {
                        self.attempt = attempt;
                        self.send(next)
                    }
                    None => State::Poor(route),
                }
            }
            (State::Poor(route), Event::UserAsksOneRung) => {
                match self.try_next(engine, self.one_rung(&route)) {
                    Some((next, attempt)) => {
                        self.attempt = attempt;
                        self.send(next)
                    }
                    None => State::Poor(route),
                }
            }
            (State::AwaitingUser(_) | State::Sent(_) | State::Poor(_), Event::UserStops) => {
                self.pending = None;
                State::Stopped
            }
            (state, event) => {
                return Err(LifecycleError::NotNow {
                    event,
                    state: state.name(),
                });
            }
        };
        self.state = next;
        Ok(())
    }

    /// Decides an attempt and moves to what it says.
    fn enter(&mut self, engine: &Engine, attempt: Attempt) {
        let decision = engine.decide_attempt(&self.profile, &attempt, self.today);
        self.attempt = attempt;
        self.explanation = decision.explanation;
        self.state = match decision.outcome {
            Outcome::Route(route) => self.send(route),
            Outcome::Refuse => State::Refused,
            Outcome::AskUser(question) => State::AwaitingUser(question),
        };
    }

    fn send(&mut self, route: Route) -> State {
        self.sent.push(route.clone());
        State::Sent(route)
    }

    /// The next attempt's route, if the engine routes it. A question or a refusal means there is
    /// nothing new to try: the explanation says why, and the request stays where it was.
    fn try_next(&mut self, engine: &Engine, attempt: Option<Attempt>) -> Option<(Route, Attempt)> {
        let attempt = attempt?;
        let decision = engine.decide_attempt(&self.profile, &attempt, self.today);
        self.explanation = decision.explanation;
        match decision.outcome {
            Outcome::Route(route) => Some((route, attempt)),
            Outcome::Refuse | Outcome::AskUser(_) => None,
        }
    }

    /// Escalation: one column right of the poor answer, any row inside the same ceiling.
    fn more_capable(&self, poor: &Route) -> Option<Attempt> {
        Some(Attempt {
            column_floor: Some(poor.cell().column.next()?),
            row_floor: None,
            ..self.attempt
        })
    }

    /// One rung further: the ceiling up one row, and a row more exposed than the poor answer's
    /// at the same column or better.
    fn one_rung(&self, poor: &Route) -> Option<Attempt> {
        Some(Attempt {
            rungs: self.attempt.rungs.checked_add(1)?,
            row_floor: Some(poor.cell().row.next()?),
            column_floor: Some(poor.cell().column),
            ..self.attempt
        })
    }

    /// The answer check judged an answer poor: escalate to the right, as the user's setting
    /// says. Never a rung: only the user takes those.
    fn screener_escalates(&mut self, engine: &Engine, poor: Route) -> State {
        let setting = engine.policy().user().escalation;
        if setting == Escalation::Off {
            return State::Poor(poor);
        }
        let Some((next, attempt)) = self.try_next(engine, self.more_capable(&poor)) else {
            return State::Poor(poor);
        };
        let first_time_out = next.leaves_local() && !self.sent.iter().any(Route::leaves_local);
        let ask = setting == Escalation::Ask
            || (setting == Escalation::AskBeforeLeavingLocal && first_time_out);
        if ask {
            self.pending = Some(attempt);
            State::AwaitingUser(Question::ApproveEscalation { route: next })
        } else {
            self.attempt = attempt;
            self.send(next)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision::{Cell, Reason};
    use crate::levels::{Category, Complexity, Sensitivity};
    use crate::policy::{AdminPolicy, Policy, RowCeilings, UserPolicy};
    use crate::provider::{Offer, ProviderAccount};
    use crate::slots::{SlotState, Slots};

    fn account(id: &str, row: Row, offers: &[(Column, u32)]) -> ProviderAccount {
        let offers = offers
            .iter()
            .map(|&(column, cost)| Offer { column, cost })
            .collect();
        ProviderAccount::new(id.parse().unwrap(), row, offers)
    }

    /// Local small and mid models, a free relay, and a no-training provider with every column.
    fn accounts() -> Vec<ProviderAccount> {
        vec![
            account("local", Row::Local, &[(Column::Small, 0), (Column::Mid, 0)]),
            account("relay-free", Row::AnonymousRelay, &[(Column::Small, 0)]),
            account(
                "provider-a",
                Row::ProviderNoTraining,
                &[(Column::Small, 2), (Column::Mid, 5), (Column::Top, 20)],
            ),
        ]
    }

    fn engine_with(user: impl FnOnce(UserPolicy) -> UserPolicy) -> Engine {
        let admin = AdminPolicy::default();
        let user = user(UserPolicy::within(&admin));
        let policy = Policy::new(admin, user).unwrap();
        Engine::new(policy, accounts(), Slots::all_healthy()).unwrap()
    }

    fn engine() -> Engine {
        engine_with(|user| user)
    }

    fn today() -> Date {
        "2026-11-15".parse().unwrap()
    }

    fn start(engine: &Engine, rating: Sensitivity, complexity: Complexity) -> Lifecycle {
        let profile = RequestProfile {
            user_rating: Some(rating),
            ..RequestProfile::new(complexity, Category::General)
        };
        Lifecycle::start(engine, profile, today())
    }

    const SCREENER_POOR: Event = Event::Answered {
        judged_by: Judge::Screener,
        good: false,
    };
    const USER_POOR: Event = Event::Answered {
        judged_by: Judge::User,
        good: false,
    };

    fn cells(life: &Lifecycle) -> Vec<(String, Cell)> {
        life.sent()
            .iter()
            .map(|r| (r.account().to_string(), r.cell()))
            .collect()
    }

    fn cell(row: Row, column: Column) -> Cell {
        Cell { row, column }
    }

    #[test]
    fn the_screener_escalates_at_home_then_asks_before_leaving() {
        let engine = engine();
        let mut life = start(&engine, Sensitivity::Personal, Complexity::Easy);
        life.handle(&engine, SCREENER_POOR).unwrap();
        // A more capable local model: no need to ask.
        assert!(matches!(life.state(), State::Sent(_)));
        life.handle(&engine, SCREENER_POOR).unwrap();
        // The next more capable model is outside: ask once.
        let State::AwaitingUser(Question::ApproveEscalation { route }) = life.state() else {
            panic!("expected to be asked, got {}", life.state());
        };
        assert_eq!(route.account().as_str(), "provider-a");
        life.handle(&engine, Event::UserApprovesEscalation).unwrap();
        assert_eq!(
            cells(&life),
            vec![
                ("local".into(), cell(Row::Local, Column::Small)),
                ("local".into(), cell(Row::Local, Column::Mid)),
                (
                    "provider-a".into(),
                    cell(Row::ProviderNoTraining, Column::Top)
                ),
            ]
        );
        assert_eq!(life.attempt().rungs, 0);
    }

    #[test]
    fn declining_an_escalation_leaves_the_poor_answer_to_the_user() {
        let engine = engine();
        let mut life = start(&engine, Sensitivity::Personal, Complexity::Medium);
        life.handle(&engine, SCREENER_POOR).unwrap();
        life.handle(&engine, Event::UserRefuses).unwrap();
        assert!(matches!(life.state(), State::Poor(r) if r.cell().column == Column::Mid));
        life.handle(&engine, Event::UserAccepts).unwrap();
        assert!(matches!(life.state(), State::Done(_)));
    }

    #[test]
    fn with_escalation_off_a_screener_verdict_waits_for_the_user() {
        let engine = engine_with(|user| UserPolicy {
            escalation: Escalation::Off,
            ..user
        });
        let mut life = start(&engine, Sensitivity::Public, Complexity::Easy);
        life.handle(&engine, SCREENER_POOR).unwrap();
        assert!(matches!(life.state(), State::Poor(_)));
        assert_eq!(life.sent().len(), 1);
    }

    #[test]
    fn the_screener_never_takes_a_rung() {
        // At the top column, the screener has nowhere to go: the answer waits for the user.
        let engine = engine();
        let mut life = start(&engine, Sensitivity::Personal, Complexity::Hard);
        life.handle(&engine, SCREENER_POOR).unwrap();
        assert!(matches!(life.state(), State::Poor(_)));
        assert_eq!(life.attempt().rungs, 0);
    }

    #[test]
    fn the_user_takes_one_rung_after_settling_for_local() {
        // A user who keeps personal requests to the relay; nothing that far offers a top model.
        let engine = engine_with(|user| UserPolicy {
            ceilings: RowCeilings {
                personal: Row::AnonymousRelay,
                ..user.ceilings
            },
            ..user
        });
        let mut life = start(&engine, Sensitivity::Personal, Complexity::Hard);
        assert!(matches!(
            life.state(),
            State::AwaitingUser(Question::SettleLocal { .. })
        ));
        life.handle(&engine, Event::UserSettles).unwrap();
        life.handle(&engine, USER_POOR).unwrap();
        life.handle(&engine, Event::UserAsksOneRung).unwrap();
        assert_eq!(
            cells(&life),
            vec![
                ("local".into(), cell(Row::Local, Column::Mid)),
                (
                    "provider-a".into(),
                    cell(Row::ProviderNoTraining, Column::Top)
                ),
            ]
        );
        assert_eq!(life.attempt().rungs, 1);
        assert!(
            life.sent()[1]
                .mechanisms()
                .contains(&crate::decision::Mechanism::Scrub)
        );
    }

    #[test]
    fn a_rung_past_the_admins_limit_leaves_the_answer_where_it_is() {
        // Personal is already at the admin's no-training limit: one rung further has nowhere to go.
        let engine = engine();
        let mut life = start(&engine, Sensitivity::Personal, Complexity::Hard);
        life.handle(&engine, USER_POOR).unwrap();
        life.handle(&engine, Event::UserAsksOneRung).unwrap();
        assert!(matches!(life.state(), State::Poor(_)));
        assert_eq!(
            life.attempt().rungs,
            0,
            "a rung that went nowhere isn't counted"
        );
        assert!(
            life.explanation()
                .reasons()
                .contains(&Reason::ExceptionAtLimit(Row::ProviderNoTraining))
        );
    }

    #[test]
    fn declining_a_lower_rating_goes_on_at_the_classifiers() {
        let engine = engine();
        let profile = RequestProfile {
            classifier_rating: Some(Sensitivity::Personal),
            user_rating: Some(Sensitivity::Public),
            ..RequestProfile::new(Complexity::Easy, Category::General)
        };
        let mut life = Lifecycle::start(&engine, profile, today());
        assert!(matches!(
            life.state(),
            State::AwaitingUser(Question::ConfirmLowerRating { .. })
        ));
        life.handle(&engine, Event::UserRefuses).unwrap();
        assert!(matches!(life.state(), State::Sent(_)));
        assert_eq!(life.profile().user_rating, None);
        assert_eq!(
            life.explanation().reasons()[0],
            Reason::Rated(
                Sensitivity::Personal,
                crate::decision::RatingSource::Classifier
            )
        );
    }

    #[test]
    fn an_event_that_does_not_fit_changes_nothing() {
        let engine = engine();
        let mut life = start(&engine, Sensitivity::Public, Complexity::Easy);
        let before = life.clone();
        assert_eq!(
            life.handle(&engine, Event::UserAsksOneRung),
            Err(LifecycleError::NotNow {
                event: Event::UserAsksOneRung,
                state: "sent"
            })
        );
        assert_eq!(life, before);
    }

    #[test]
    fn without_an_answer_check_only_the_user_judges() {
        let slots = Slots::all_healthy().with(Slot::AnswerCheck, SlotState::Empty);
        let engine = Engine::new(Policy::default(), accounts(), slots).unwrap();
        let mut life = start(&engine, Sensitivity::Public, Complexity::Easy);
        assert_eq!(
            life.handle(&engine, SCREENER_POOR),
            Err(LifecycleError::NoAnswerCheck)
        );
        life.handle(&engine, USER_POOR).unwrap();
        assert!(matches!(life.state(), State::Poor(_)));
    }
}
