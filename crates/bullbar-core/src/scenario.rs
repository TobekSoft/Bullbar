//! Hand-written decision scenarios (TODO §1.7): a request, a setup, and the decision expected.
//!
//! A scenario file holds several scenarios on one theme:
//!
//! ```toml
//! today = 2026-11-15            # for review dates; a scenario may set its own
//!
//! [[scenario]]
//! name = "hard public work goes to the cheapest top model"
//! edition = "casual"            # a directory under config/
//! request = { sensitivity = "public", complexity = "hard" }
//! expect = { outcome = "route", account = "provider-b", row = "provider-standard", column = "top" }
//! ```
//!
//! A request takes `sensitivity`, `classifier`, `complexity`, `category` (default `general`),
//! `credentials`, `row_limit` and `want`; an `attempt` takes `confirm_lower` and `rungs`; `slots`
//! takes `empty` and `unhealthy` lists, applied over the edition's. Expected `outcome` is `route`,
//! `ask`, `refuse` or `no-start`; `account`, `row` and `column` check the route (or the local route
//! a settle question offers); `question` is `settle-local`, `confirm-lower-rating` or
//! `approve-escalation`; `mechanisms` is the exact set engaged; each of `reasons` must appear in
//! the explanation, and none of `reasons_absent`.
//!
//! Core reads no files: the caller hands in each file's text and each edition's config.

use serde::Deserialize;

use crate::config::Config;
use crate::decision::{Outcome, Question, Route};
use crate::levels::{Category, Column, Complexity, Row, Sensitivity, is_simple_name};
use crate::lifecycle::Attempt;
use crate::profile::RequestProfile;
use crate::provider::{AccountId, Date};
use crate::slots::{Slot, SlotState};

/// A scenario file that didn't parse.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ScenarioError(String);

/// One scenario, ready to run.
#[derive(Debug, Clone)]
pub struct Scenario {
    /// What it shows, in a sentence.
    pub name: String,
    /// The sample config it runs on: a directory under `config/`.
    pub edition: String,
    today: Date,
    request: RequestProfile,
    attempt: Attempt,
    empty: Vec<Slot>,
    unhealthy: Vec<Slot>,
    expect: Expect,
}

/// What running a scenario showed.
#[derive(Debug, Clone)]
pub struct Checked {
    /// The scenario's name.
    pub name: String,
    /// Each way the decision differed from what was expected. Empty means it passed.
    pub failures: Vec<String>,
    /// The decision as a person would read it.
    pub decision: String,
}

impl Checked {
    /// True when the decision was as expected.
    pub fn passed(&self) -> bool {
        self.failures.is_empty()
    }
}

/// Parses a scenario file.
pub fn parse(text: &str) -> Result<Vec<Scenario>, ScenarioError> {
    let file: FileEntry =
        toml::from_str(text).map_err(|e| ScenarioError(e.to_string().trim_end().to_owned()))?;
    file.scenario
        .into_iter()
        .map(|entry| {
            let fail =
                |problem: &str| ScenarioError(format!("scenario `{}`: {problem}", entry.name));
            if !is_simple_name(&entry.edition) {
                return Err(fail("`edition` must be a directory name under config/"));
            }
            let today = entry
                .today
                .or(file.today)
                .ok_or_else(|| fail("no `today`, in the scenario or at the top of the file"))?;
            let r = entry.request;
            let request = RequestProfile {
                user_rating: r.sensitivity,
                classifier_rating: r.classifier,
                credentials_found: r.credentials,
                row_limit: r.row_limit,
                wanted_column: r.want,
                pinned_to: r.pinned,
                ..RequestProfile::new(r.complexity, r.category.unwrap_or(Category::General))
            };
            let a = entry.attempt.unwrap_or_default();
            let attempt = Attempt {
                lower_rating_confirmed: a.confirm_lower,
                rungs: a.rungs,
                ..Attempt::default()
            };
            let slots = entry.slots.unwrap_or_default();
            Ok(Scenario {
                name: entry.name,
                edition: entry.edition,
                today,
                request,
                attempt,
                empty: slots.empty,
                unhealthy: slots.unhealthy,
                expect: entry.expect,
            })
        })
        .collect()
}

impl Scenario {
    /// Runs the scenario on its edition's config.
    pub fn run(&self, config: &Config) -> Checked {
        let mut config = config.clone();
        for &slot in &self.empty {
            config.slots = config.slots.with(slot, SlotState::Empty);
        }
        for &slot in &self.unhealthy {
            config.slots = config.slots.with(slot, SlotState::Unhealthy);
        }
        let mut failures = Vec::new();
        let expect = &self.expect;
        let engine = match config.engine() {
            Ok(engine) => engine,
            Err(e) => {
                if expect.outcome != Expected::NoStart {
                    failures.push(format!("expected {}, but {e}", expect.outcome.name()));
                }
                return self.checked(failures, format!("won't start: {e}"));
            }
        };
        let decision = engine.decide_attempt(&self.request, &self.attempt, self.today);
        let rendered = format!("{}\n{}", decision.outcome, decision.explanation);

        let (got, route, question) = match &decision.outcome {
            Outcome::Route(route) => (Expected::Route, Some(route), None),
            Outcome::Refuse => (Expected::Refuse, None, None),
            Outcome::AskUser(question) => {
                let offered = match question {
                    Question::SettleLocal { route, .. } | Question::MovePin { route, .. } => {
                        Some(route)
                    }
                    _ => None,
                };
                (Expected::Ask, offered, Some(question_kind(question)))
            }
        };
        if got != expect.outcome {
            failures.push(format!(
                "expected {}, got {}",
                expect.outcome.name(),
                got.name()
            ));
        }
        if let Some(want) = expect.question
            && question != Some(want)
        {
            failures.push(format!("expected the question {}", want.name()));
        }
        self.check_route(route, &mut failures);
        let mentions = |text: &str| {
            decision
                .explanation
                .reasons()
                .iter()
                .any(|r| r.to_string().contains(text))
        };
        for reason in &expect.reasons {
            if !mentions(reason) {
                failures.push(format!("no reason containing \"{reason}\""));
            }
        }
        for reason in &expect.reasons_absent {
            if mentions(reason) {
                failures.push(format!("a reason contains \"{reason}\""));
            }
        }
        self.checked(failures, rendered)
    }

    fn check_route(&self, route: Option<&Route>, failures: &mut Vec<String>) {
        let expect = &self.expect;
        let wants_route = expect.account.is_some()
            || expect.row.is_some()
            || expect.column.is_some()
            || expect.mechanisms.is_some();
        let Some(route) = route else {
            if wants_route {
                failures.push("expected a route to check, got none".to_owned());
            }
            return;
        };
        if let Some(account) = &expect.account
            && route.account() != account
        {
            failures.push(format!(
                "expected account {account}, got {}",
                route.account()
            ));
        }
        if let Some(row) = expect.row
            && route.cell().row != row
        {
            failures.push(format!("expected row {row}, got {}", route.cell().row));
        }
        if let Some(column) = expect.column
            && route.cell().column != column
        {
            failures.push(format!(
                "expected column {column}, got {}",
                route.cell().column
            ));
        }
        if let Some(want) = &expect.mechanisms {
            let mut got: Vec<String> = route
                .mechanisms()
                .iter()
                .map(|m| m.to_string().replace(' ', "-"))
                .collect();
            let mut want = want.clone();
            got.sort();
            want.sort();
            if got != want {
                failures.push(format!(
                    "expected mechanisms [{}], got [{}]",
                    want.join(", "),
                    got.join(", ")
                ));
            }
        }
    }

    fn checked(&self, failures: Vec<String>, decision: String) -> Checked {
        Checked {
            name: self.name.clone(),
            failures,
            decision,
        }
    }
}

fn question_kind(question: &Question) -> QuestionKind {
    match question {
        Question::ConfirmLowerRating { .. } => QuestionKind::ConfirmLowerRating,
        Question::SettleLocal { .. } => QuestionKind::SettleLocal,
        Question::ApproveEscalation { .. } => QuestionKind::ApproveEscalation,
        Question::MovePin { .. } => QuestionKind::MovePin,
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileEntry {
    today: Option<Date>,
    #[serde(default)]
    scenario: Vec<ScenarioEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScenarioEntry {
    name: String,
    edition: String,
    today: Option<Date>,
    request: RequestEntry,
    attempt: Option<AttemptEntry>,
    slots: Option<SlotsEntry>,
    expect: Expect,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestEntry {
    sensitivity: Option<Sensitivity>,
    classifier: Option<Sensitivity>,
    complexity: Complexity,
    category: Option<Category>,
    #[serde(default)]
    credentials: bool,
    row_limit: Option<Row>,
    want: Option<Column>,
    /// The account the request's topic is pinned to.
    pinned: Option<AccountId>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct AttemptEntry {
    #[serde(default)]
    confirm_lower: bool,
    #[serde(default)]
    rungs: u8,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct SlotsEntry {
    #[serde(default)]
    empty: Vec<Slot>,
    #[serde(default)]
    unhealthy: Vec<Slot>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Expect {
    outcome: Expected,
    account: Option<AccountId>,
    row: Option<Row>,
    column: Option<Column>,
    question: Option<QuestionKind>,
    mechanisms: Option<Vec<String>>,
    #[serde(default)]
    reasons: Vec<String>,
    #[serde(default)]
    reasons_absent: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Expected {
    Route,
    Ask,
    Refuse,
    NoStart,
}

impl Expected {
    fn name(self) -> &'static str {
        match self {
            Expected::Route => "a route",
            Expected::Ask => "a question",
            Expected::Refuse => "a refusal",
            Expected::NoStart => "the engine not to start",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum QuestionKind {
    SettleLocal,
    ConfirmLowerRating,
    ApproveEscalation,
    MovePin,
}

impl QuestionKind {
    fn name(self) -> &'static str {
        match self {
            QuestionKind::SettleLocal => "settle-local",
            QuestionKind::ConfirmLowerRating => "confirm-lower-rating",
            QuestionKind::ApproveEscalation => "approve-escalation",
            QuestionKind::MovePin => "move-pin",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scenario_needs_a_date() {
        let text = r#"
            [[scenario]]
            name = "undated"
            edition = "casual"
            request = { complexity = "easy" }
            expect = { outcome = "route" }
        "#;
        assert!(parse(text).unwrap_err().to_string().contains("no `today`"));
    }

    #[test]
    fn an_edition_is_a_plain_name() {
        let text = r#"
            today = 2026-11-15
            [[scenario]]
            name = "escapes"
            edition = "../secrets"
            request = { complexity = "easy" }
            expect = { outcome = "route" }
        "#;
        assert!(
            parse(text)
                .unwrap_err()
                .to_string()
                .contains("directory name")
        );
    }

    #[test]
    fn a_typo_in_an_expectation_is_an_error() {
        let text = r#"
            today = 2026-11-15
            [[scenario]]
            name = "typo"
            edition = "casual"
            request = { complexity = "easy" }
            expect = { outcome = "route", acount = "local" }
        "#;
        assert!(parse(text).unwrap_err().to_string().contains("acount"));
    }
}
