//! What the engine returns: an outcome, and always the reasons behind it.

use std::fmt;

use crate::eligibility::{CloudEligible, LocalOnly};
use crate::levels::{Category, Column, Complexity, Row, Sensitivity};
use crate::provider::{AccountId, Date};
use crate::slots::Slot;

/// A cell of the routing grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Cell {
    /// Who sees the request.
    pub row: Row,
    /// How capable the model is.
    pub column: Column,
}

impl fmt::Display for Cell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} / {}", self.row, self.column)
    }
}

/// A protection switched on for a request, beyond the always-on floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Mechanism {
    /// Remove personal details and relationships on the way out.
    Scrub,
    /// Put scrubbed details back into the reply.
    Restore,
    /// Keep the request's topic on the provider it is pinned to.
    TopicPinning,
    /// Send under the provider's persona.
    Personas,
    /// Add plausible but irrelevant material inside the request, stripped from the reply
    /// (ADR 0015). No rating engages it yet: its cost is weighed when obfuscation is built.
    DecoyContent,
    /// Additional plausible but irrelevant queries, sent as their own requests.
    Chaff,
    /// Record what the provider has now seen.
    ExposureLedger,
    /// Replace credentials with placeholders before sending (ADR 0009).
    StripCredentials,
}

impl fmt::Display for Mechanism {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Mechanism::Scrub => "scrub",
            Mechanism::Restore => "restore",
            Mechanism::TopicPinning => "topic pinning",
            Mechanism::Personas => "personas",
            Mechanism::DecoyContent => "decoy content",
            Mechanism::Chaff => "chaff",
            Mechanism::ExposureLedger => "exposure ledger",
            Mechanism::StripCredentials => "strip credentials",
        })
    }
}

/// Where a request goes. A route to any row other than Local carries a [`CloudEligible`], so it
/// cannot be built for a Secret-rated request or one still holding credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    account: AccountId,
    cell: Cell,
    mechanisms: Vec<Mechanism>,
    cloud: Option<CloudEligible>,
}

impl Route {
    /// A route that stays on the local layer.
    pub(crate) fn local(account: AccountId, column: Column, mechanisms: Vec<Mechanism>) -> Self {
        Route {
            account,
            cell: Cell {
                row: Row::Local,
                column,
            },
            mechanisms,
            cloud: None,
        }
    }

    /// A route to any row, with proof the request may leave the local layer.
    pub(crate) fn cloud(
        account: AccountId,
        cell: Cell,
        mechanisms: Vec<Mechanism>,
        eligible: CloudEligible,
    ) -> Self {
        Route {
            account,
            cell,
            mechanisms,
            cloud: Some(eligible),
        }
    }

    /// Switches on the mechanisms for a route that leaves the local layer.
    pub(crate) fn engage(&mut self, mechanisms: Vec<Mechanism>) {
        self.mechanisms = mechanisms;
    }

    /// The account the request goes to.
    pub fn account(&self) -> &AccountId {
        &self.account
    }

    /// The cell it lands in.
    pub fn cell(&self) -> Cell {
        self.cell
    }

    /// The mechanisms switched on for it.
    pub fn mechanisms(&self) -> &[Mechanism] {
        &self.mechanisms
    }

    /// True when the request leaves the local layer.
    pub fn leaves_local(&self) -> bool {
        self.cell.row != Row::Local
    }
}

/// What the engine needs the user to decide before it can route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Question {
    /// The user rated the request below the classifier; confirm?
    ConfirmLowerRating {
        /// The classifier's rating.
        classifier: Sensitivity,
        /// The user's rating.
        user: Sensitivity,
    },
    /// Nothing allowed is capable enough; settle for the best local answer, labelled as such?
    SettleLocal {
        /// The best local route on offer.
        route: Route,
        /// The column the work needs.
        wanted: Column,
    },
    /// The answer check judged the last answer poor; send it on to this more capable model?
    ApproveEscalation {
        /// Where it would go next.
        route: Route,
    },
    /// The topic is pinned to an account that can't take this request; move the topic to
    /// another (ADR 0021)? Never done without asking.
    MovePin {
        /// The topic.
        category: Category,
        /// The account it is pinned to.
        from: AccountId,
        /// Where it would go instead, and the topic with it.
        route: Route,
    },
}

impl fmt::Display for Question {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Question::ConfirmLowerRating { classifier, user } => write!(
                f,
                "The classifier rated this {classifier}; you rated it {user}. Send it as {user}?"
            ),
            Question::SettleLocal { route, wanted } => write!(
                f,
                "Nothing allowed offers a {wanted} model. Settle for {}'s {} model on the \
                 local layer, or refuse?",
                route.account(),
                route.cell().column
            ),
            Question::MovePin {
                category,
                from,
                route,
            } => write!(
                f,
                "Your {category} topic is pinned to {from}, which can't take this request now. \
                 Move the topic to {} (its later requests go there too), or refuse?",
                route.account()
            ),
            Question::ApproveEscalation { route } => write!(
                f,
                "The answer looked poor. Try {route}{}?",
                if route.leaves_local() {
                    ", outside the local layer"
                } else {
                    ""
                }
            ),
        }
    }
}

/// The engine's verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Send it here.
    Route(Route),
    /// Don't send it anywhere.
    Refuse,
    /// Ask the user first.
    AskUser(Question),
}

impl fmt::Display for Route {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at {}", self.account, self.cell)?;
        if !self.mechanisms.is_empty() {
            let names: Vec<String> = self.mechanisms.iter().map(|m| m.to_string()).collect();
            write!(f, " (with {})", names.join(", "))?;
        }
        Ok(())
    }
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Outcome::Route(route) => write!(f, "route: {route}"),
            Outcome::Refuse => f.write_str("refuse"),
            Outcome::AskUser(question) => write!(f, "ask: {question}"),
        }
    }
}

/// Where a sensitivity rating came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RatingSource {
    /// The classifier.
    Classifier,
    /// The user.
    User,
    /// Nobody rated it, so it is treated as Sensitive.
    Unrated,
}

/// One step of an explanation. New rules add variants here, each with its sentence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    /// The rating the engine went by, and its source.
    Rated(Sensitivity, RatingSource),
    /// The user rated below the classifier and has confirmed it.
    LowerRatingConfirmed {
        /// The classifier's rating, now overridden.
        classifier: Sensitivity,
    },
    /// The user rated below the classifier without confirming.
    LowerRatingNeedsConfirmation,
    /// The user rated below the classifier, which isn't allowed here, so the classifier's stands.
    LowerRatingNotAllowed {
        /// The classifier's rating, which stands.
        classifier: Sensitivity,
        /// The locked category that forbids it, or `None` when the admin forbids it everywhere.
        locked: Option<Category>,
    },
    /// A category ceiling tightens the limit.
    CategoryCeiling(Category, Row),
    /// The request limits itself to this row.
    RequestLimit(Row),
    /// The local-only shutter is closed (the kill switch): nothing leaves.
    ShutterClosed,
    /// The request asks for at least this column.
    ColumnRequested(Column),
    /// A one-off exception took the ceiling up the ladder, for this request only (ADR 0011).
    ExceptionTaken {
        /// Rungs asked for.
        rungs: u8,
        /// The ceiling before.
        from: Row,
        /// The ceiling after.
        to: Row,
    },
    /// The exception stopped at the admin's limit.
    ExceptionAtLimit(Row),
    /// No exception for this request, and why.
    ExceptionNotAllowed(ExceptionBlock),
    /// The work wants a more capable column than the cap allows; the best within the cap is used.
    CapabilityCap {
        /// The column the work wants.
        wanted: Column,
        /// The cap.
        cap: Column,
    },
    /// The request may not leave the local layer.
    LocalOnly(LocalOnly),
    /// The most exposed row a rating allows.
    Ceiling(Sensitivity, Row),
    /// The least capable column the work needs (ADR 0008 mapping).
    MinimumColumn(Complexity, Column),
    /// The topic is pinned to this account: other cloud accounts are left out (ADR 0021).
    Pinned(Category, AccountId),
    /// The account the topic is pinned to can't take this request.
    PinUnavailable(Category, AccountId),
    /// The cell picked: the cheapest inside both limits.
    Chose {
        /// The account.
        account: AccountId,
        /// The cell.
        cell: Cell,
        /// Its relative cost.
        cost: u32,
    },
    /// No account offers a capable enough model within the allowed rows.
    NoCellAllowed {
        /// The most exposed row allowed.
        ceiling: Row,
        /// The least capable column needed.
        minimum: Column,
    },
    /// No local model either, so there is nothing to settle for.
    NoLocalFallback,
    /// The protections switched on because the request leaves the local layer at this rating.
    Protections(Sensitivity, Vec<Mechanism>),
    /// Credentials will be replaced by placeholders before the request leaves (ADR 0009).
    CredentialsStripped,
    /// No PII scrubber, so Personal and Sensitive requests stay local.
    NoScrubber,
    /// No local runner, and Sensitive requests may not leave without one (ADR 0014).
    NoLocalRunnerForSensitive,
    /// No local runner, so this Sensitive request can only be answered outside, scrubbed (ADR 0014).
    SensitiveLeavesWithoutLocal,
    /// No provider adapters, so no cloud rows.
    NoProviderAdapters,
    /// No terms register, so every provider is treated as standard terms.
    NoTermsRegister,
    /// An empty optional slot means these protections can't be switched on.
    ProtectionsUnavailable(Slot, Vec<Mechanism>),
    /// No local runner to write chaff, and the user hasn't asked for canned chaff (ADR 0015).
    ChaffNeedsLocalRunner,
    /// The chosen account counts one row more exposed until it is reviewed (ADR 0010 §8).
    Demoted {
        /// The account.
        account: AccountId,
        /// Its row as last reviewed.
        from: Row,
        /// The row it counts as today.
        to: Row,
        /// Why.
        cause: Demotion,
    },
    /// After a poor answer: only models at least this capable (ADR 0013 §4).
    ColumnFloor(Column),
    /// After a poor answer, one rung further: only rows at least this exposed (ADR 0013 §4).
    RowFloor(Row),
    /// The exposure ledger skipped an account: it has seen enough of this category.
    LedgerSkipped {
        /// The account.
        account: AccountId,
        /// The request's category.
        category: Category,
        /// How many it has seen.
        seen: u32,
    },
}

/// Why an account counts one row more exposed than its reviewed row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Demotion {
    /// Marked demoted: something changed and nobody has reviewed it yet.
    Status,
    /// Its review was due on this date and hasn't been done.
    Overdue(Date),
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Reason::Rated(s, RatingSource::Classifier) => write!(f, "rated {s} by the classifier"),
            Reason::Rated(s, RatingSource::User) => write!(f, "rated {s} by you"),
            Reason::Rated(s, RatingSource::Unrated) => {
                write!(f, "not rated, so treated as {s}")
            }
            Reason::LocalOnly(LocalOnly::SecretRating) => {
                f.write_str("rated secret, so it stays on the local layer")
            }
            Reason::LocalOnly(LocalOnly::Credentials) => f.write_str(
                "contains credentials and strip-and-send is off, so it stays on the local layer",
            ),
            Reason::LowerRatingConfirmed { classifier } => {
                write!(
                    f,
                    "you confirmed rating it below the classifier's {classifier}"
                )
            }
            Reason::LowerRatingNeedsConfirmation => {
                f.write_str("rating below the classifier needs your confirmation")
            }
            Reason::LowerRatingNotAllowed {
                classifier,
                locked: Some(category),
            } => write!(
                f,
                "the {category} category is locked, so the classifier's {classifier} stands"
            ),
            Reason::LowerRatingNotAllowed {
                classifier,
                locked: None,
            } => write!(
                f,
                "rating below the classifier isn't allowed, so its {classifier} stands"
            ),
            Reason::CategoryCeiling(category, row) => {
                write!(f, "{category} requests may go no further than {row}")
            }
            Reason::RequestLimit(row) => write!(f, "this request is limited to {row}"),
            Reason::ShutterClosed => {
                f.write_str("the shutter is closed, so nothing leaves the local layer")
            }
            Reason::ColumnRequested(column) => {
                write!(f, "you asked for at least a {column} model")
            }
            Reason::ExceptionTaken { rungs, from, to } => write!(
                f,
                "one-off exception, {rungs} rung{} up: from {from} to {to}, for this request only",
                if *rungs == 1 { "" } else { "s" }
            ),
            Reason::ExceptionAtLimit(row) => {
                write!(
                    f,
                    "the admin's limit is {row}, so the exception stops there"
                )
            }
            Reason::ExceptionNotAllowed(ExceptionBlock::AdminForbids) => {
                f.write_str("one-off exceptions aren't allowed here")
            }
            Reason::ExceptionNotAllowed(ExceptionBlock::UserTurnedOff) => {
                f.write_str("one-off exceptions are turned off in your settings")
            }
            Reason::ExceptionNotAllowed(ExceptionBlock::Locked(category)) => write!(
                f,
                "the {category} category is locked, so no one-off exception"
            ),
            Reason::ExceptionNotAllowed(ExceptionBlock::LocalOnly) => {
                f.write_str("no one-off exception moves secret content or credentials")
            }
            Reason::CapabilityCap { wanted, cap } => {
                write!(f, "the work wants a {wanted} model, but the cap is {cap}")
            }
            Reason::Ceiling(s @ Sensitivity::Public, row) => {
                write!(f, "{s} requests may go as far as {row}")
            }
            Reason::Ceiling(s, row) => {
                write!(f, "{s} requests may go as far as {row}, once scrubbed")
            }
            Reason::MinimumColumn(c, col) => write!(f, "{c} work needs at least a {col} model"),
            Reason::Pinned(category, account) => {
                write!(
                    f,
                    "the {category} topic is pinned to {account}: other providers left out"
                )
            }
            Reason::PinUnavailable(category, account) => write!(
                f,
                "{account}, which the {category} topic is pinned to, can't take this request"
            ),
            Reason::Chose {
                account,
                cell,
                cost,
            } => write!(f, "cheapest allowed: {account} at {cell} (cost {cost})"),
            Reason::NoCellAllowed { ceiling, minimum } => write!(
                f,
                "no account offers a {minimum} model or better within {ceiling}"
            ),
            Reason::NoLocalFallback => f.write_str("no local model to settle for"),
            Reason::Protections(s, mechanisms) => {
                let names: Vec<String> = mechanisms.iter().map(|m| m.to_string()).collect();
                write!(
                    f,
                    "leaves the local layer rated {s}, so: {}",
                    names.join(", ")
                )
            }
            Reason::CredentialsStripped => f.write_str(
                "credentials will be replaced by placeholders before it leaves (strip-and-send)",
            ),
            Reason::NoScrubber => {
                f.write_str("no working PII scrubber, so it stays on the local layer")
            }
            Reason::SensitiveLeavesWithoutLocal => f.write_str(
                "no working local runner, so this sensitive request can only be answered outside, \
                 scrubbed",
            ),
            Reason::NoLocalRunnerForSensitive => f.write_str(
                "no working local runner, and sensitive requests may not leave without one",
            ),
            Reason::NoProviderAdapters => {
                f.write_str("no working provider adapters, so no cloud rows")
            }
            Reason::NoTermsRegister => {
                f.write_str("no working terms register, so every provider counts as standard terms")
            }
            Reason::ProtectionsUnavailable(slot, mechanisms) => {
                let names: Vec<String> = mechanisms.iter().map(|m| m.to_string()).collect();
                write!(f, "no working {slot}, so without: {}", names.join(", "))
            }
            Reason::ChaffNeedsLocalRunner => f.write_str(
                "no working local runner to write chaff, so without: chaff \
                 (turn on canned chaff with chaff_without_local)",
            ),
            Reason::Demoted {
                account,
                from,
                to,
                cause,
            } => {
                let cause = match cause {
                    Demotion::Status => "has changes nobody has reviewed".to_owned(),
                    Demotion::Overdue(date) => format!("was due for review on {date}"),
                };
                write!(f, "{account} {cause}, so it counts as {to}, not {from}")
            }
            Reason::ColumnFloor(column) => {
                write!(f, "after a poor answer, only a {column} model or better")
            }
            Reason::RowFloor(row) => {
                write!(f, "one rung further: only {row} or more exposed")
            }
            Reason::LedgerSkipped {
                account,
                category,
                seen,
            } => write!(
                f,
                "{account} skipped: it has already seen {seen} {category} requests"
            ),
        }
    }
}

/// Why a one-off exception doesn't apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExceptionBlock {
    /// The admin doesn't allow exceptions.
    AdminForbids,
    /// The user has turned exceptions off.
    UserTurnedOff,
    /// The request's category is locked.
    Locked(Category),
    /// Secret-rated content, or credentials still in the request.
    LocalOnly,
}

/// The ordered reasons behind a decision. Every decision has one; there are no silent decisions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Explanation(Vec<Reason>);

impl Explanation {
    /// Adds the next reason.
    pub fn push(&mut self, reason: Reason) {
        self.0.push(reason);
    }

    /// The reasons, in order.
    pub fn reasons(&self) -> &[Reason] {
        &self.0
    }
}

impl fmt::Display for Explanation {
    /// One numbered line per reason.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, reason) in self.0.iter().enumerate() {
            writeln!(f, "{}. {reason}", i + 1)?;
        }
        Ok(())
    }
}

/// An outcome with its explanation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    /// What to do.
    pub outcome: Outcome,
    /// Why.
    pub explanation: Explanation,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eligibility::CredentialState;

    fn account(name: &str) -> AccountId {
        name.parse().unwrap()
    }

    #[test]
    fn local_routes_stay_on_the_local_row() {
        let route = Route::local(account("local"), Column::Mid, vec![]);
        assert_eq!(
            route.cell(),
            Cell {
                row: Row::Local,
                column: Column::Mid
            }
        );
        assert!(!route.leaves_local());
    }

    #[test]
    fn cloud_routes_need_proof_of_eligibility() {
        let eligible = CloudEligible::check(Sensitivity::Personal, CredentialState::None).unwrap();
        let cell = Cell {
            row: Row::ProviderNoTraining,
            column: Column::Top,
        };
        let route = Route::cloud(
            account("provider-a"),
            cell,
            vec![Mechanism::Scrub],
            eligible,
        );
        assert!(route.leaves_local());
        assert_eq!(route.mechanisms(), &[Mechanism::Scrub]);
    }

    #[test]
    fn explanations_render_as_numbered_lines() {
        let mut explanation = Explanation::default();
        explanation.push(Reason::Rated(Sensitivity::Sensitive, RatingSource::Unrated));
        explanation.push(Reason::LocalOnly(LocalOnly::Credentials));
        assert_eq!(
            explanation.to_string(),
            "1. not rated, so treated as sensitive\n\
             2. contains credentials and strip-and-send is off, so it stays on the local layer\n"
        );
    }
}
