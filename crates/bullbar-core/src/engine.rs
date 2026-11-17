//! The decision engine: where a request may go, and why.

use crate::capabilities::{Capabilities, Capability};
use crate::decision::{
    Cell, Decision, Demotion, ExceptionBlock, Explanation, Mechanism, Outcome, Question,
    RatingSource, Reason, Route,
};
use crate::eligibility::{CloudEligible, CredentialState};
use crate::ledger::Ledger;
use crate::levels::{Column, Row, Sensitivity};
use crate::lifecycle::Attempt;
use crate::policy::Policy;
use crate::profile::RequestProfile;
use crate::provider::{AccountId, AccountStatus, Date, ProviderAccount};
use crate::slots::{Slot, SlotState, Slots};

/// Why the engine won't start.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EngineError {
    /// Required slots that are empty or unhealthy (ADR 0002).
    #[error("required slots not ready: {}", describe(.0))]
    RequiredSlots(Vec<(Slot, SlotState)>),
}

fn describe(slots: &[(Slot, SlotState)]) -> String {
    slots
        .iter()
        .map(|(slot, state)| format!("{slot} ({state})"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Decides routes for requests, within a policy, over a set of accounts.
#[derive(Debug, Clone)]
pub struct Engine {
    policy: Policy,
    /// The accounts the installed slots can reach, at the rows they count as.
    accounts: Vec<ProviderAccount>,
    slots: Slots,
    /// What the slots can actually do, given what each capability depends on (ADR 0002).
    capabilities: Capabilities,
    ledger: Ledger,
}

/// An account in the running for one request, at the row it counts as today.
#[derive(Clone, Copy)]
struct InPlay<'a> {
    account: &'a ProviderAccount,
    row: Row,
    demotion: Option<Demotion>,
}

/// A cell on offer, with what decides the pick: cost, then exposure, then capability, then name.
struct Candidate {
    key: (u32, Row, Column, String),
    route: Route,
}

impl Engine {
    /// An engine for this policy, these accounts and these slots. Refuses to start while any
    /// required slot is empty or unhealthy. Accounts the slots can't reach are dropped: local
    /// ones without a local runner, cloud ones without provider adapters; without a terms
    /// register, every cloud account counts as standard terms. Accounts marked out are dropped.
    pub fn new(
        policy: Policy,
        accounts: Vec<ProviderAccount>,
        slots: Slots,
    ) -> Result<Self, EngineError> {
        let missing = slots.missing_required();
        if !missing.is_empty() {
            return Err(EngineError::RequiredSlots(missing));
        }
        let accounts = accounts
            .into_iter()
            .filter_map(|mut account| {
                if account.status == AccountStatus::Out {
                    return None;
                }
                if account.row == Row::Local {
                    return slots.usable(Slot::LocalRunner).then_some(account);
                }
                if !slots.usable(Slot::ProviderAdapters) {
                    return None;
                }
                if !slots.usable(Slot::TermsRegister) {
                    account.row = Row::ProviderStandard;
                }
                Some(account)
            })
            .collect();
        let capabilities = Capabilities::new(&slots);
        Ok(Engine {
            policy,
            accounts,
            slots,
            capabilities,
            ledger: Ledger::default(),
        })
    }

    /// This engine with an exposure ledger. It only counts while the exposure ledger slot works.
    pub fn with_ledger(mut self, ledger: Ledger) -> Self {
        self.ledger = ledger;
        self
    }

    /// The policy it decides within.
    pub fn policy(&self) -> &Policy {
        &self.policy
    }

    /// True when this slot is filled and healthy.
    pub fn slot_usable(&self, slot: Slot) -> bool {
        self.slots.usable(slot)
    }

    /// What the installed slots can actually do.
    pub fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    /// Decides where a new request goes, on `today`'s date (for review dates). Always explains
    /// why.
    pub fn decide(&self, profile: &RequestProfile, today: Date) -> Decision {
        self.decide_attempt(profile, &Attempt::default(), today)
    }

    /// Decides one attempt at a request: the first, or a retry after a poor answer, carrying what
    /// the user has confirmed and the floors that keep each retry on a new cell (ADR 0013).
    pub fn decide_attempt(
        &self,
        profile: &RequestProfile,
        attempt: &Attempt,
        today: Date,
    ) -> Decision {
        let mut explanation = Explanation::default();
        let outcome = self.outcome(profile, attempt, today, &mut explanation);
        Decision {
            outcome,
            explanation,
        }
    }

    fn outcome(
        &self,
        profile: &RequestProfile,
        attempt: &Attempt,
        today: Date,
        why: &mut Explanation,
    ) -> Outcome {
        let lowering = if self.policy.is_locked(&profile.category) {
            Lowering::Locked(profile.category.clone())
        } else if self.policy.admin().lower_ratings_allowed {
            Lowering::Allowed
        } else {
            Lowering::Forbidden
        };
        let rating = match resolve_rating(profile, attempt.lower_rating_confirmed, lowering, why) {
            Ok(rating) => rating,
            Err(question) => return Outcome::AskUser(question),
        };
        // With no local model, Sensitive can't stay home: it leaves scrubbed, or is refused where
        // the user or the admin has turned that off (ADR 0014). The scrubber check still applies.
        if rating == Sensitivity::Sensitive && !self.slots.usable(Slot::LocalRunner) {
            if !self.policy.user().send_sensitive_without_local {
                why.push(Reason::NoLocalRunnerForSensitive);
                return Outcome::Refuse;
            }
            why.push(Reason::SensitiveLeavesWithoutLocal);
        }

        let credentials = match (profile.credentials_found, self.policy.user().strip_and_send) {
            (false, _) => CredentialState::None,
            (true, false) => CredentialState::Found,
            (true, true) => CredentialState::Stripped,
        };
        let eligible = match CloudEligible::check(rating, credentials) {
            Ok(eligible) => Some(eligible),
            Err(local_only) => {
                why.push(Reason::LocalOnly(local_only));
                None
            }
        };
        let mut ceiling = match eligible {
            Some(_) => {
                let row = self.policy.user().ceilings.for_rating(rating);
                why.push(Reason::Ceiling(rating, row));
                match self.policy.category_ceiling(&profile.category) {
                    Some(category_row) if category_row < row => {
                        why.push(Reason::CategoryCeiling(
                            profile.category.clone(),
                            category_row,
                        ));
                        category_row
                    }
                    _ => row,
                }
            }
            None => Row::Local,
        };
        if attempt.rungs > 0 {
            let rungs = attempt.rungs;
            ceiling = self.exception(profile, rungs, rating, eligible.is_some(), ceiling, why);
        }
        if let Some(limit) = profile.row_limit
            && limit < ceiling
        {
            why.push(Reason::RequestLimit(limit));
            ceiling = limit;
        }
        if profile.shutter_closed && ceiling > Row::Local {
            why.push(Reason::ShutterClosed);
            ceiling = Row::Local;
        }
        let needs_scrubbing = matches!(rating, Sensitivity::Personal | Sensitivity::Sensitive);
        if needs_scrubbing && ceiling > Row::Local && !self.slots.usable(Slot::PiiScrubber) {
            why.push(Reason::NoScrubber);
            ceiling = Row::Local;
        }
        if ceiling > Row::Local {
            if !self.slots.usable(Slot::ProviderAdapters) {
                why.push(Reason::NoProviderAdapters);
            } else if !self.slots.usable(Slot::TermsRegister) {
                why.push(Reason::NoTermsRegister);
            }
        }

        let mut minimum = self
            .policy
            .admin()
            .minimum_columns
            .for_complexity(profile.complexity);
        why.push(Reason::MinimumColumn(profile.complexity, minimum));
        if let Some(wanted) = profile.wanted_column
            && wanted > minimum
        {
            why.push(Reason::ColumnRequested(wanted));
            minimum = wanted;
        }
        let cap = self.policy.user().max_column;
        if minimum > cap {
            why.push(Reason::CapabilityCap {
                wanted: minimum,
                cap,
            });
            minimum = cap;
        }
        // Floors come after the cap: past the cap there is nothing more capable to try.
        if let Some(floor) = attempt.column_floor
            && floor > minimum
        {
            why.push(Reason::ColumnFloor(floor));
            minimum = floor;
        }
        let row_floor = attempt.row_floor.unwrap_or(Row::Local);
        if row_floor > Row::Local {
            why.push(Reason::RowFloor(row_floor));
        }

        let in_play = self.in_play(profile, today, ceiling, why);
        let floor = Cell {
            row: row_floor,
            column: minimum,
        };
        let pin = self.pin(profile, rating, eligible.is_some());
        let chosen = match pin {
            None => cheapest(&in_play, ceiling, floor, cap, eligible),
            Some(pinned) => {
                // Only the pinned account may take it off the local layer.
                let kept: Vec<InPlay<'_>> = in_play
                    .iter()
                    .copied()
                    .filter(|p| p.account.row == Row::Local || p.account.id == *pinned)
                    .collect();
                match cheapest(&kept, ceiling, floor, cap, eligible) {
                    Some(chosen) => {
                        if chosen.route.leaves_local() {
                            why.push(Reason::Pinned(profile.category.clone(), pinned.clone()));
                        }
                        Some(chosen)
                    }
                    None => {
                        // Somewhere else could take it: ask, never move quietly.
                        if let Some(other) = cheapest(&in_play, ceiling, floor, cap, eligible)
                            .filter(|c| c.route.leaves_local())
                        {
                            why.push(Reason::PinUnavailable(
                                profile.category.clone(),
                                pinned.clone(),
                            ));
                            let route = self.finish(other, &in_play, rating, credentials, why);
                            return Outcome::AskUser(Question::MovePin {
                                category: profile.category.clone(),
                                from: pinned.clone(),
                                route,
                            });
                        }
                        None
                    }
                }
            }
        };
        if let Some(chosen) = chosen {
            let route = self.finish(chosen, &in_play, rating, credentials, why);
            return Outcome::Route(route);
        }

        why.push(Reason::NoCellAllowed { ceiling, minimum });
        match self.best_local(cap) {
            Some(route) => Outcome::AskUser(Question::SettleLocal {
                route,
                wanted: minimum,
            }),
            None => {
                why.push(Reason::NoLocalFallback);
                Outcome::Refuse
            }
        }
    }

    /// A one-off exception (ADR 0011): the ceiling moves up one row per rung, for this request
    /// only, never past the admin's limit. None for Secret or credentials still present, a locked
    /// category, or where the admin or the user has turned exceptions off. Scrubbing still applies:
    /// the scrubber check comes after this.
    fn exception(
        &self,
        profile: &RequestProfile,
        rungs: u8,
        rating: Sensitivity,
        eligible: bool,
        ceiling: Row,
        why: &mut Explanation,
    ) -> Row {
        let block = if !eligible {
            Some(ExceptionBlock::LocalOnly)
        } else if self.policy.is_locked(&profile.category) {
            Some(ExceptionBlock::Locked(profile.category.clone()))
        } else if !self.policy.admin().exceptions_allowed {
            Some(ExceptionBlock::AdminForbids)
        } else if !self.policy.user().exceptions {
            Some(ExceptionBlock::UserTurnedOff)
        } else {
            None
        };
        if let Some(block) = block {
            why.push(Reason::ExceptionNotAllowed(block));
            return ceiling;
        }
        let limit = self.policy.admin_ceiling(rating, &profile.category);
        let stepped = ceiling.up(rungs);
        let to = stepped.min(limit).max(ceiling);
        if to > ceiling {
            why.push(Reason::ExceptionTaken {
                rungs,
                from: ceiling,
                to,
            });
        }
        if stepped > limit {
            why.push(Reason::ExceptionAtLimit(limit));
        }
        to
    }

    /// The protections the slots can provide, dependencies included; the rest are named in the
    /// explanation, each under the first slot its chain lacks.
    fn available(&self, wanted: Vec<Mechanism>, why: &mut Explanation) -> Vec<Mechanism> {
        let mut missing: Vec<(Slot, Vec<Mechanism>)> = Vec::new();
        let mut kept = Vec::new();
        let mut chaff_off = false;
        for mechanism in wanted {
            let lacks =
                Capability::of(mechanism).and_then(|cap| self.capabilities.missing_slot(cap));
            match lacks {
                // Without a local runner, chaff is canned and easy to filter out: off unless the
                // user asked for it (ADR 0015).
                None if mechanism == Mechanism::Chaff
                    && !self.slots.usable(Slot::LocalRunner)
                    && !self.policy.user().chaff_without_local =>
                {
                    chaff_off = true;
                }
                Some(slot) => match missing.iter_mut().find(|(s, _)| *s == slot) {
                    Some((_, list)) => list.push(mechanism),
                    None => missing.push((slot, vec![mechanism])),
                },
                None => kept.push(mechanism),
            }
        }
        for (slot, mechanisms) in missing {
            why.push(Reason::ProtectionsUnavailable(slot, mechanisms));
        }
        if chaff_off {
            why.push(Reason::ChaffNeedsLocalRunner);
        }
        kept
    }

    /// The pin that applies to this request: its topic's account, when the request may leave, is
    /// rated Personal or above, and topic pinning works (the federation slot; without it, "one
    /// account per provider, no topic pinning").
    fn pin<'p>(
        &self,
        profile: &'p RequestProfile,
        rating: Sensitivity,
        may_leave: bool,
    ) -> Option<&'p AccountId> {
        let pinning = self
            .capabilities
            .missing_slot(Capability::TopicPinning)
            .is_none();
        (may_leave && pinning && rating >= Sensitivity::Personal)
            .then_some(profile.pinned_to.as_ref())
            .flatten()
    }

    /// The chosen cell as a route, with its reasons and, if it leaves, its protections.
    fn finish(
        &self,
        chosen: Candidate,
        in_play: &[InPlay<'_>],
        rating: Sensitivity,
        credentials: CredentialState,
        why: &mut Explanation,
    ) -> Route {
        let (cost, ..) = chosen.key;
        let mut route = chosen.route;
        if let Some(play) = in_play.iter().find(|p| p.account.id == *route.account())
            && let Some(cause) = play.demotion
        {
            why.push(Reason::Demoted {
                account: play.account.id.clone(),
                from: play.account.row,
                to: play.row,
                cause,
            });
        }
        why.push(Reason::Chose {
            account: route.account().clone(),
            cell: route.cell(),
            cost,
        });
        // Local first: an answer from home needs no scrubbing, persona or restore.
        if route.leaves_local() {
            let mechanisms = self.available(protections(rating, credentials), why);
            if credentials == CredentialState::Stripped {
                why.push(Reason::CredentialsStripped);
            }
            if !mechanisms.is_empty() {
                why.push(Reason::Protections(rating, mechanisms.clone()));
            }
            route.engage(mechanisms);
        }
        route
    }

    /// The accounts in the running for this request, at the rows they count as today. A cloud
    /// account demoted, or past its review date, counts one row more exposed (standard terms
    /// stay standard): unreviewed changes demote, never promote. Local accounts have no terms to
    /// review. With a working exposure ledger, cloud accounts that have seen enough of this
    /// category are skipped; those within the ceiling are named.
    fn in_play(
        &self,
        profile: &RequestProfile,
        today: Date,
        ceiling: Row,
        why: &mut Explanation,
    ) -> Vec<InPlay<'_>> {
        let ledger = self.slots.usable(Slot::ExposureLedger);
        self.accounts
            .iter()
            .filter_map(|account| {
                if account.row == Row::Local {
                    return Some(InPlay {
                        account,
                        row: Row::Local,
                        demotion: None,
                    });
                }
                let demotion = match (account.status, account.review_due) {
                    (AccountStatus::Demoted, _) => Some(Demotion::Status),
                    (_, Some(due)) if due < today => Some(Demotion::Overdue(due)),
                    _ => None,
                };
                let row = match demotion {
                    Some(_) => account.row.up(1),
                    None => account.row,
                };
                if ledger && let Some(seen) = self.ledger.exceeded(&account.id, &profile.category) {
                    if row <= ceiling {
                        why.push(Reason::LedgerSkipped {
                            account: account.id.clone(),
                            category: profile.category.clone(),
                            seen,
                        });
                    }
                    return None;
                }
                Some(InPlay {
                    account,
                    row,
                    demotion: demotion.filter(|_| row > account.row),
                })
            })
            .collect()
    }

    /// The most capable local model within the cap, cheapest first among equals: what to offer
    /// when nothing allowed is capable enough.
    fn best_local(&self, cap: Column) -> Option<Route> {
        self.accounts
            .iter()
            .filter(|account| account.row == Row::Local)
            .flat_map(|account| account.offers.iter().map(move |offer| (account, offer)))
            .filter(|(_, offer)| offer.column <= cap)
            .max_by(|(a1, o1), (a2, o2)| {
                (
                    o1.column,
                    std::cmp::Reverse(o1.cost),
                    std::cmp::Reverse(&a1.id),
                )
                    .cmp(&(
                        o2.column,
                        std::cmp::Reverse(o2.cost),
                        std::cmp::Reverse(&a2.id),
                    ))
            })
            .map(|(account, offer)| Route::local(account.id.clone(), offer.column, vec![]))
    }
}

/// The cheapest cell from the floor (a minimum column, and a minimum row on a retry one rung
/// further) up to the ceiling and the cap, at the rows the accounts count as today. A cell off
/// the local row is only on offer with proof of eligibility.
fn cheapest(
    in_play: &[InPlay<'_>],
    ceiling: Row,
    floor: Cell,
    cap: Column,
    eligible: Option<CloudEligible>,
) -> Option<Candidate> {
    let minimum = floor.column;
    in_play
        .iter()
        .filter(|play| play.row >= floor.row && play.row <= ceiling)
        .flat_map(|play| {
            play.account
                .offers
                .iter()
                .filter(move |offer| offer.column >= minimum && offer.column <= cap)
                .filter_map(move |offer| {
                    let id = play.account.id.clone();
                    let cell = Cell {
                        row: play.row,
                        column: offer.column,
                    };
                    let route = match (play.row, eligible) {
                        (Row::Local, _) => Route::local(id, offer.column, vec![]),
                        (_, Some(eligible)) => Route::cloud(id, cell, vec![], eligible),
                        (_, None) => return None,
                    };
                    Some(Candidate {
                        key: (
                            offer.cost,
                            play.row,
                            offer.column,
                            play.account.id.to_string(),
                        ),
                        route,
                    })
                })
        })
        .min_by(|a, b| a.key.cmp(&b.key))
}

/// What a request leaving the local layer gets, by rating (concept design, "Sensitivity ratings
/// and fast paths"). Public gets the always-on floor only, which is not listed here.
fn protections(rating: Sensitivity, credentials: CredentialState) -> Vec<Mechanism> {
    let mut mechanisms = Vec::new();
    if rating >= Sensitivity::Personal {
        mechanisms.extend([
            Mechanism::Scrub,
            Mechanism::Restore,
            Mechanism::TopicPinning,
        ]);
    }
    if rating >= Sensitivity::Sensitive {
        mechanisms.extend([
            Mechanism::Personas,
            Mechanism::Chaff,
            Mechanism::ExposureLedger,
        ]);
    }
    if credentials == CredentialState::Stripped {
        // Credentials are never put back into a reply (ADR 0009): no Restore here.
        mechanisms.push(Mechanism::StripCredentials);
    }
    mechanisms.sort();
    mechanisms.dedup();
    mechanisms
}

/// Whether a request may be rated below the classifier.
enum Lowering {
    /// Yes, once confirmed.
    Allowed,
    /// No: the admin forbids it everywhere.
    Forbidden,
    /// No: the request's category is locked.
    Locked(crate::levels::Category),
}

/// The rating the engine goes by. The user may rate higher freely; lower than the classifier
/// only once confirmed, and only where allowed. Unrated means Sensitive. A classifier rating that
/// is present counts even if the classifier slot is unhealthy: ignoring it could only loosen.
fn resolve_rating(
    profile: &RequestProfile,
    confirmed: bool,
    lowering: Lowering,
    why: &mut Explanation,
) -> Result<Sensitivity, Question> {
    match (profile.user_rating, profile.classifier_rating) {
        (Some(user), Some(classifier)) if user < classifier => {
            let locked = match lowering {
                Lowering::Allowed => None,
                Lowering::Forbidden => Some(None),
                Lowering::Locked(category) => Some(Some(category)),
            };
            if let Some(locked) = locked {
                why.push(Reason::Rated(classifier, RatingSource::Classifier));
                why.push(Reason::LowerRatingNotAllowed { classifier, locked });
                Ok(classifier)
            } else if confirmed {
                why.push(Reason::Rated(user, RatingSource::User));
                why.push(Reason::LowerRatingConfirmed { classifier });
                Ok(user)
            } else {
                why.push(Reason::Rated(classifier, RatingSource::Classifier));
                why.push(Reason::LowerRatingNeedsConfirmation);
                Err(Question::ConfirmLowerRating { classifier, user })
            }
        }
        (Some(user), _) => {
            why.push(Reason::Rated(user, RatingSource::User));
            Ok(user)
        }
        (None, Some(classifier)) => {
            why.push(Reason::Rated(classifier, RatingSource::Classifier));
            Ok(classifier)
        }
        (None, None) => {
            why.push(Reason::Rated(Sensitivity::Sensitive, RatingSource::Unrated));
            Ok(Sensitivity::Sensitive)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::levels::{Category, Complexity};
    use crate::policy::{AdminPolicy, CategoryRule, RowCeilings, UserPolicy};
    use crate::provider::Offer;
    use std::collections::BTreeMap;

    /// An engine with every slot healthy.
    fn build(policy: Policy, accounts: Vec<ProviderAccount>) -> Engine {
        Engine::new(policy, accounts, Slots::all_healthy()).expect("all slots are healthy")
    }

    /// An engine with every slot healthy except one, left empty.
    fn without(slot: Slot) -> Engine {
        let slots = Slots::all_healthy().with(slot, SlotState::Empty);
        Engine::new(Policy::default(), accounts(), slots).expect("only an optional slot is empty")
    }

    fn account(id: &str, row: Row, offers: &[(Column, u32)]) -> ProviderAccount {
        ProviderAccount::new(
            id.parse().unwrap(),
            row,
            offers
                .iter()
                .map(|&(column, cost)| Offer { column, cost })
                .collect(),
        )
    }

    /// The day the tests run on.
    fn today() -> Date {
        "2026-11-15".parse().unwrap()
    }

    /// A local runner with small and mid models, a free relay, and two providers.
    fn accounts() -> Vec<ProviderAccount> {
        vec![
            account("local", Row::Local, &[(Column::Small, 0), (Column::Mid, 0)]),
            account("relay-free", Row::AnonymousRelay, &[(Column::Small, 0)]),
            account(
                "provider-a",
                Row::ProviderNoTraining,
                &[(Column::Small, 2), (Column::Mid, 5), (Column::Top, 20)],
            ),
            account(
                "provider-b",
                Row::ProviderStandard,
                &[(Column::Small, 1), (Column::Mid, 3), (Column::Top, 10)],
            ),
        ]
    }

    fn engine() -> Engine {
        build(Policy::default(), accounts())
    }

    fn rated(rating: Sensitivity, complexity: Complexity) -> RequestProfile {
        RequestProfile {
            user_rating: Some(rating),
            ..RequestProfile::new(complexity, Category::General)
        }
    }

    fn routed(decision: &Decision) -> (String, Cell) {
        match &decision.outcome {
            Outcome::Route(route) => (route.account().to_string(), route.cell()),
            other => panic!("expected a route, got {other}"),
        }
    }

    fn cell(row: Row, column: Column) -> Cell {
        Cell { row, column }
    }

    /// The outcome line and the explanation, as a person would read them.
    fn render(decision: &Decision) -> String {
        format!("{}\n{}", decision.outcome, decision.explanation)
    }

    #[test]
    fn a_free_local_cell_beats_a_free_cloud_cell() {
        let d = engine().decide(&rated(Sensitivity::Public, Complexity::Easy), today());
        assert_eq!(
            routed(&d),
            ("local".into(), cell(Row::Local, Column::Small))
        );
    }

    #[test]
    fn hard_public_work_goes_to_the_cheapest_top_model() {
        let d = engine().decide(&rated(Sensitivity::Public, Complexity::Hard), today());
        assert_eq!(
            routed(&d),
            (
                "provider-b".into(),
                cell(Row::ProviderStandard, Column::Top)
            )
        );
        insta::assert_snapshot!(render(&d));
    }

    #[test]
    fn unrated_requests_are_treated_as_sensitive() {
        let d = engine().decide(
            &RequestProfile::new(Complexity::Medium, Category::General),
            today(),
        );
        assert_eq!(routed(&d), ("local".into(), cell(Row::Local, Column::Mid)));
        assert_eq!(
            d.explanation.reasons()[0],
            Reason::Rated(Sensitivity::Sensitive, RatingSource::Unrated)
        );
    }

    #[test]
    fn secret_stays_local_whatever_the_policy() {
        let open = Policy::from_admin(AdminPolicy {
            ceilings: RowCeilings {
                public: Row::ProviderStandard,
                personal: Row::ProviderStandard,
                sensitive: Row::ProviderStandard,
            },
            ..AdminPolicy::default()
        });
        let d =
            build(open, accounts()).decide(&rated(Sensitivity::Secret, Complexity::Easy), today());
        assert_eq!(
            routed(&d),
            ("local".into(), cell(Row::Local, Column::Small))
        );
    }

    #[test]
    fn credentials_keep_a_public_request_local() {
        let profile = RequestProfile {
            credentials_found: true,
            ..rated(Sensitivity::Public, Complexity::Medium)
        };
        let d = engine().decide(&profile, today());
        assert_eq!(routed(&d), ("local".into(), cell(Row::Local, Column::Mid)));
    }

    #[test]
    fn hard_and_secret_asks_to_settle_for_the_best_local_model() {
        let d = engine().decide(&rated(Sensitivity::Secret, Complexity::Hard), today());
        match &d.outcome {
            Outcome::AskUser(Question::SettleLocal { route, wanted }) => {
                assert_eq!(route.cell(), cell(Row::Local, Column::Mid));
                assert_eq!(*wanted, Column::Top);
            }
            other => panic!("expected to ask about settling, got {other}"),
        }
        insta::assert_snapshot!(render(&d));
    }

    #[test]
    fn with_no_local_model_secret_is_refused() {
        let cloud_only = accounts()
            .into_iter()
            .filter(|a| a.row != Row::Local)
            .collect();
        let d = build(Policy::default(), cloud_only)
            .decide(&rated(Sensitivity::Secret, Complexity::Easy), today());
        assert_eq!(d.outcome, Outcome::Refuse);
        assert_eq!(
            d.explanation.reasons().last(),
            Some(&Reason::NoLocalFallback)
        );
    }

    #[test]
    fn rating_below_the_classifier_asks_first() {
        let profile = RequestProfile {
            classifier_rating: Some(Sensitivity::Sensitive),
            ..rated(Sensitivity::Public, Complexity::Hard)
        };
        let d = engine().decide(&profile, today());
        assert_eq!(
            d.outcome,
            Outcome::AskUser(Question::ConfirmLowerRating {
                classifier: Sensitivity::Sensitive,
                user: Sensitivity::Public
            })
        );
        insta::assert_snapshot!(render(&d));
    }

    #[test]
    fn a_confirmed_lower_rating_is_used() {
        let profile = RequestProfile {
            classifier_rating: Some(Sensitivity::Sensitive),
            ..rated(Sensitivity::Public, Complexity::Hard)
        };
        let d = engine().decide_attempt(&profile, &confirmed(), today());
        assert_eq!(
            routed(&d),
            (
                "provider-b".into(),
                cell(Row::ProviderStandard, Column::Top)
            )
        );
    }

    #[test]
    fn rating_above_the_classifier_needs_no_confirmation() {
        let profile = RequestProfile {
            classifier_rating: Some(Sensitivity::Public),
            ..rated(Sensitivity::Secret, Complexity::Easy)
        };
        let d = engine().decide(&profile, today());
        assert_eq!(
            routed(&d),
            ("local".into(), cell(Row::Local, Column::Small))
        );
    }

    fn mechanisms(decision: &Decision) -> Vec<Mechanism> {
        match &decision.outcome {
            Outcome::Route(route) => route.mechanisms().to_vec(),
            other => panic!("expected a route, got {other}"),
        }
    }

    #[test]
    fn personal_requests_leave_only_scrubbed() {
        let d = engine().decide(&rated(Sensitivity::Personal, Complexity::Hard), today());
        assert_eq!(
            routed(&d),
            (
                "provider-a".into(),
                cell(Row::ProviderNoTraining, Column::Top)
            )
        );
        assert_eq!(
            mechanisms(&d),
            [
                Mechanism::Scrub,
                Mechanism::Restore,
                Mechanism::TopicPinning
            ]
        );
        insta::assert_snapshot!(render(&d));
    }

    #[test]
    fn sensitive_requests_get_the_full_set_when_they_leave() {
        let d = engine().decide(&rated(Sensitivity::Sensitive, Complexity::Hard), today());
        assert_eq!(
            mechanisms(&d),
            [
                Mechanism::Scrub,
                Mechanism::Restore,
                Mechanism::TopicPinning,
                Mechanism::Personas,
                Mechanism::Chaff,
                Mechanism::ExposureLedger
            ]
        );
    }

    #[test]
    fn answers_from_home_need_no_protections() {
        let d = engine().decide(&rated(Sensitivity::Sensitive, Complexity::Medium), today());
        assert_eq!(routed(&d), ("local".into(), cell(Row::Local, Column::Mid)));
        assert!(mechanisms(&d).is_empty());
    }

    #[test]
    fn public_requests_leave_with_the_floor_only() {
        let d = engine().decide(&rated(Sensitivity::Public, Complexity::Hard), today());
        assert!(mechanisms(&d).is_empty());
    }

    #[test]
    fn strip_and_send_lets_the_rest_of_the_request_leave() {
        let admin = AdminPolicy::default();
        let user = UserPolicy {
            strip_and_send: true,
            ..UserPolicy::within(&admin)
        };
        let policy = Policy::new(admin, user).unwrap();
        let profile = RequestProfile {
            credentials_found: true,
            ..rated(Sensitivity::Public, Complexity::Hard)
        };
        let d = build(policy, accounts()).decide(&profile, today());
        assert_eq!(
            routed(&d),
            (
                "provider-b".into(),
                cell(Row::ProviderStandard, Column::Top)
            )
        );
        assert_eq!(mechanisms(&d), [Mechanism::StripCredentials]);
        insta::assert_snapshot!(render(&d));
    }

    #[test]
    fn a_missing_required_slot_stops_the_engine_starting() {
        let slots = Slots::all_healthy()
            .with(Slot::OutboundCheck, SlotState::Empty)
            .with(Slot::Shutters, SlotState::Unhealthy);
        let err = Engine::new(Policy::default(), accounts(), slots).unwrap_err();
        assert_eq!(
            err.to_string(),
            "required slots not ready: outbound check (empty), shutters (unhealthy)"
        );
    }

    #[test]
    fn the_required_slots_alone_are_enough_to_start() {
        assert!(Engine::new(Policy::default(), accounts(), Slots::required_only()).is_ok());
    }

    #[test]
    fn without_a_scrubber_personal_requests_stay_local() {
        let d = without(Slot::PiiScrubber)
            .decide(&rated(Sensitivity::Personal, Complexity::Hard), today());
        assert!(matches!(
            d.outcome,
            Outcome::AskUser(Question::SettleLocal { .. })
        ));
        assert!(d.explanation.reasons().contains(&Reason::NoScrubber));
    }

    #[test]
    fn without_a_scrubber_public_requests_still_leave() {
        let d = without(Slot::PiiScrubber)
            .decide(&rated(Sensitivity::Public, Complexity::Hard), today());
        assert_eq!(
            routed(&d),
            (
                "provider-b".into(),
                cell(Row::ProviderStandard, Column::Top)
            )
        );
    }

    #[test]
    fn without_a_local_runner_sensitive_requests_are_refused_where_turned_off() {
        // Before ADR 0014 this was the only behaviour; now it is the user's or the admin's choice.
        let admin = AdminPolicy {
            send_sensitive_without_local_allowed: false,
            ..AdminPolicy::default()
        };
        let slots = Slots::all_healthy().with(Slot::LocalRunner, SlotState::Empty);
        let engine = Engine::new(Policy::from_admin(admin), accounts(), slots).unwrap();
        let d = engine.decide(&rated(Sensitivity::Sensitive, Complexity::Easy), today());
        assert_eq!(d.outcome, Outcome::Refuse);
        insta::assert_snapshot!(render(&d));
    }

    #[test]
    fn without_a_local_runner_sensitive_requests_leave_scrubbed() {
        let d = without(Slot::LocalRunner)
            .decide(&rated(Sensitivity::Sensitive, Complexity::Easy), today());
        assert_eq!(
            routed(&d),
            (
                "relay-free".into(),
                cell(Row::AnonymousRelay, Column::Small)
            )
        );
        assert!(mechanisms(&d).contains(&Mechanism::Scrub));
        insta::assert_snapshot!(render(&d));
    }

    #[test]
    fn without_a_local_runner_canned_chaff_goes_only_when_asked_for() {
        let admin = AdminPolicy::default();
        let user = UserPolicy {
            chaff_without_local: true,
            ..UserPolicy::within(&admin)
        };
        let slots = Slots::all_healthy().with(Slot::LocalRunner, SlotState::Empty);
        let engine = Engine::new(Policy::new(admin, user).unwrap(), accounts(), slots).unwrap();
        let d = engine.decide(&rated(Sensitivity::Sensitive, Complexity::Easy), today());
        assert!(mechanisms(&d).contains(&Mechanism::Chaff));
        assert!(!mechanisms(&d).contains(&Mechanism::Personas));
        let d = without(Slot::LocalRunner)
            .decide(&rated(Sensitivity::Sensitive, Complexity::Easy), today());
        assert!(!mechanisms(&d).contains(&Mechanism::Chaff));
    }

    #[test]
    fn without_a_local_runner_or_a_scrubber_sensitive_requests_are_refused() {
        let slots = Slots::all_healthy()
            .with(Slot::LocalRunner, SlotState::Empty)
            .with(Slot::PiiScrubber, SlotState::Empty);
        let engine = Engine::new(Policy::default(), accounts(), slots).unwrap();
        let d = engine.decide(&rated(Sensitivity::Sensitive, Complexity::Easy), today());
        assert_eq!(d.outcome, Outcome::Refuse);
        assert!(d.explanation.reasons().contains(&Reason::NoScrubber));
    }

    #[test]
    fn without_a_local_runner_easy_public_work_goes_out() {
        let d = without(Slot::LocalRunner)
            .decide(&rated(Sensitivity::Public, Complexity::Easy), today());
        assert_eq!(
            routed(&d),
            (
                "relay-free".into(),
                cell(Row::AnonymousRelay, Column::Small)
            )
        );
    }

    #[test]
    fn without_provider_adapters_nothing_leaves() {
        let d = without(Slot::ProviderAdapters)
            .decide(&rated(Sensitivity::Public, Complexity::Hard), today());
        assert!(matches!(
            d.outcome,
            Outcome::AskUser(Question::SettleLocal { .. })
        ));
        assert!(
            d.explanation
                .reasons()
                .contains(&Reason::NoProviderAdapters)
        );
    }

    #[test]
    fn without_a_terms_register_every_provider_counts_as_standard() {
        let d = without(Slot::TermsRegister)
            .decide(&rated(Sensitivity::Personal, Complexity::Hard), today());
        // provider-a's no-training row is unearned, so personal requests can't reach it.
        assert!(matches!(
            d.outcome,
            Outcome::AskUser(Question::SettleLocal { .. })
        ));
        assert!(d.explanation.reasons().contains(&Reason::NoTermsRegister));
    }

    #[test]
    fn an_unhealthy_classifiers_rating_still_counts() {
        let slots = Slots::all_healthy().with(Slot::Classifier, SlotState::Unhealthy);
        let profile = RequestProfile {
            classifier_rating: Some(Sensitivity::Secret),
            ..RequestProfile::new(Complexity::Hard, Category::General)
        };
        let d = Engine::new(Policy::default(), accounts(), slots)
            .unwrap()
            .decide(&profile, today());
        assert_eq!(
            d.explanation.reasons()[0],
            Reason::Rated(Sensitivity::Secret, RatingSource::Classifier)
        );
    }

    #[test]
    fn missing_protection_slots_are_named() {
        let slots = Slots::all_healthy()
            .with(Slot::Obfuscator, SlotState::Empty)
            .with(Slot::Federation, SlotState::Unhealthy);
        let d = Engine::new(Policy::default(), accounts(), slots)
            .unwrap()
            .decide(&rated(Sensitivity::Sensitive, Complexity::Hard), today());
        assert_eq!(
            mechanisms(&d),
            [
                Mechanism::Scrub,
                Mechanism::Restore,
                Mechanism::ExposureLedger
            ]
        );
        insta::assert_snapshot!(render(&d));
    }

    fn health_rule(ceiling: Row, locked: bool) -> AdminPolicy {
        AdminPolicy {
            categories: BTreeMap::from([(Category::Health, CategoryRule { ceiling, locked })]),
            ..AdminPolicy::default()
        }
    }

    fn health(rating: Sensitivity, complexity: Complexity) -> RequestProfile {
        RequestProfile {
            category: Category::Health,
            ..rated(rating, complexity)
        }
    }

    #[test]
    fn a_category_ceiling_tightens_the_limit() {
        let engine = build(
            Policy::from_admin(health_rule(Row::AnonymousRelay, false)),
            accounts(),
        );
        let d = engine.decide(&health(Sensitivity::Public, Complexity::Easy), today());
        // Easy work is answered at home either way.
        assert_eq!(
            routed(&d),
            ("local".into(), cell(Row::Local, Column::Small))
        );
        // Hard work would go to provider-b, but that is past the health ceiling.
        let d = engine.decide(&health(Sensitivity::Public, Complexity::Hard), today());
        assert!(matches!(
            d.outcome,
            Outcome::AskUser(Question::SettleLocal { .. })
        ));
        assert!(d.explanation.reasons().contains(&Reason::CategoryCeiling(
            Category::Health,
            Row::AnonymousRelay
        )));
        insta::assert_snapshot!(render(&d));
    }

    #[test]
    fn other_categories_are_not_affected_by_a_category_ceiling() {
        let engine = build(
            Policy::from_admin(health_rule(Row::Local, true)),
            accounts(),
        );
        let d = engine.decide(&rated(Sensitivity::Public, Complexity::Hard), today());
        assert_eq!(
            routed(&d),
            (
                "provider-b".into(),
                cell(Row::ProviderStandard, Column::Top)
            )
        );
    }

    #[test]
    fn a_locked_category_keeps_the_classifiers_rating() {
        let engine = build(
            Policy::from_admin(health_rule(Row::ProviderStandard, true)),
            accounts(),
        );
        let profile = RequestProfile {
            classifier_rating: Some(Sensitivity::Sensitive),
            ..health(Sensitivity::Public, Complexity::Hard)
        };
        let d = engine.decide_attempt(&profile, &confirmed(), today());
        assert_eq!(
            d.explanation.reasons()[1],
            Reason::LowerRatingNotAllowed {
                classifier: Sensitivity::Sensitive,
                locked: Some(Category::Health)
            }
        );
        insta::assert_snapshot!(render(&d));
    }

    #[test]
    fn an_admin_can_forbid_lowering_ratings_everywhere() {
        let admin = AdminPolicy {
            lower_ratings_allowed: false,
            ..AdminPolicy::default()
        };
        let profile = RequestProfile {
            classifier_rating: Some(Sensitivity::Secret),
            ..rated(Sensitivity::Public, Complexity::Easy)
        };
        let d = build(Policy::from_admin(admin), accounts()).decide_attempt(
            &profile,
            &confirmed(),
            today(),
        );
        assert_eq!(
            routed(&d),
            ("local".into(), cell(Row::Local, Column::Small))
        );
        assert_eq!(
            d.explanation.reasons()[1],
            Reason::LowerRatingNotAllowed {
                classifier: Sensitivity::Secret,
                locked: None
            }
        );
    }

    #[test]
    fn the_capability_cap_clamps_with_a_reason() {
        let admin = AdminPolicy::default();
        let user = UserPolicy {
            max_column: Column::Mid,
            ..UserPolicy::within(&admin)
        };
        let engine = build(Policy::new(admin, user).unwrap(), accounts());
        let d = engine.decide(&rated(Sensitivity::Public, Complexity::Hard), today());
        assert_eq!(routed(&d), ("local".into(), cell(Row::Local, Column::Mid)));
        insta::assert_snapshot!(render(&d));
    }

    #[test]
    fn the_cap_also_bounds_cheaper_columns_above_the_minimum() {
        // Only a top model is on offer, and it is free; a cap of small rules it out.
        let admin = AdminPolicy {
            max_column: Column::Small,
            ..AdminPolicy::default()
        };
        let accounts = vec![account("local", Row::Local, &[(Column::Top, 0)])];
        let d = build(Policy::from_admin(admin), accounts)
            .decide(&rated(Sensitivity::Public, Complexity::Easy), today());
        assert_eq!(d.outcome, Outcome::Refuse);
    }

    /// A user who keeps Personal requests to the relay, under an admin who allows no-training
    /// providers.
    fn cautious_user(admin: AdminPolicy) -> Engine {
        let user = UserPolicy {
            ceilings: RowCeilings {
                personal: Row::AnonymousRelay,
                ..admin.ceilings
            },
            ..UserPolicy::within(&admin)
        };
        build(Policy::new(admin, user).unwrap(), accounts())
    }

    /// A retry taking this many one-off rungs.
    fn rungs(rungs: u8) -> Attempt {
        Attempt {
            rungs,
            ..Attempt::default()
        }
    }

    /// The user has confirmed a rating below the classifier's.
    fn confirmed() -> Attempt {
        Attempt {
            lower_rating_confirmed: true,
            ..Attempt::default()
        }
    }

    #[test]
    fn a_one_off_exception_moves_one_rung_still_scrubbed() {
        let engine = cautious_user(AdminPolicy::default());
        let first = engine.decide(&rated(Sensitivity::Personal, Complexity::Hard), today());
        assert!(matches!(
            first.outcome,
            Outcome::AskUser(Question::SettleLocal { .. })
        ));
        let d = engine.decide_attempt(
            &rated(Sensitivity::Personal, Complexity::Hard),
            &rungs(1),
            today(),
        );
        assert_eq!(
            routed(&d),
            (
                "provider-a".into(),
                cell(Row::ProviderNoTraining, Column::Top)
            )
        );
        assert!(mechanisms(&d).contains(&Mechanism::Scrub));
        insta::assert_snapshot!(render(&d));
    }

    #[test]
    fn an_exception_stops_at_the_admins_limit() {
        let engine = cautious_user(AdminPolicy::default());
        let d = engine.decide_attempt(
            &rated(Sensitivity::Personal, Complexity::Hard),
            &rungs(3),
            today(),
        );
        assert_eq!(
            routed(&d),
            (
                "provider-a".into(),
                cell(Row::ProviderNoTraining, Column::Top)
            )
        );
        assert!(
            d.explanation
                .reasons()
                .contains(&Reason::ExceptionAtLimit(Row::ProviderNoTraining))
        );
    }

    fn blocked(d: &Decision) -> Option<ExceptionBlock> {
        d.explanation.reasons().iter().find_map(|r| match r {
            Reason::ExceptionNotAllowed(block) => Some(block.clone()),
            _ => None,
        })
    }

    #[test]
    fn no_exception_where_the_admin_forbids_it() {
        let engine = build(
            Policy::from_admin(AdminPolicy {
                exceptions_allowed: false,
                ceilings: RowCeilings {
                    personal: Row::AnonymousRelay,
                    ..RowCeilings::default()
                },
                ..AdminPolicy::default()
            }),
            accounts(),
        );
        let d = engine.decide_attempt(
            &rated(Sensitivity::Personal, Complexity::Hard),
            &rungs(1),
            today(),
        );
        assert!(!matches!(d.outcome, Outcome::Route(_)));
        assert_eq!(blocked(&d), Some(ExceptionBlock::AdminForbids));
    }

    #[test]
    fn no_exception_when_the_user_turned_them_off() {
        let admin = AdminPolicy::default();
        let user = UserPolicy {
            exceptions: false,
            ceilings: RowCeilings {
                personal: Row::AnonymousRelay,
                ..admin.ceilings
            },
            ..UserPolicy::within(&admin)
        };
        let engine = build(Policy::new(admin, user).unwrap(), accounts());
        let d = engine.decide_attempt(
            &rated(Sensitivity::Personal, Complexity::Hard),
            &rungs(1),
            today(),
        );
        assert_eq!(blocked(&d), Some(ExceptionBlock::UserTurnedOff));
    }

    #[test]
    fn no_exception_in_a_locked_category() {
        let engine = build(
            Policy::from_admin(health_rule(Row::AnonymousRelay, true)),
            accounts(),
        );
        let d = engine.decide_attempt(
            &health(Sensitivity::Public, Complexity::Hard),
            &rungs(1),
            today(),
        );
        assert!(!matches!(d.outcome, Outcome::Route(_)));
        assert_eq!(blocked(&d), Some(ExceptionBlock::Locked(Category::Health)));
    }

    #[test]
    fn no_exception_moves_secret_content() {
        let d = engine().decide_attempt(
            &rated(Sensitivity::Secret, Complexity::Easy),
            &rungs(2),
            today(),
        );
        assert_eq!(
            routed(&d),
            ("local".into(), cell(Row::Local, Column::Small))
        );
        assert_eq!(blocked(&d), Some(ExceptionBlock::LocalOnly));
    }

    #[test]
    fn a_request_can_keep_itself_local() {
        let profile = RequestProfile {
            row_limit: Some(Row::Local),
            ..rated(Sensitivity::Public, Complexity::Hard)
        };
        let d = engine().decide(&profile, today());
        assert!(matches!(
            d.outcome,
            Outcome::AskUser(Question::SettleLocal { .. })
        ));
        assert!(
            d.explanation
                .reasons()
                .contains(&Reason::RequestLimit(Row::Local))
        );
    }

    #[test]
    fn a_request_limit_beats_an_exception() {
        let profile = RequestProfile {
            row_limit: Some(Row::AnonymousRelay),
            ..rated(Sensitivity::Personal, Complexity::Easy)
        };
        let d = cautious_user(AdminPolicy::default()).decide_attempt(&profile, &rungs(2), today());
        assert!(routed(&d).1.row <= Row::AnonymousRelay);
    }

    #[test]
    fn a_request_can_ask_for_a_more_capable_model() {
        let profile = RequestProfile {
            wanted_column: Some(Column::Top),
            ..rated(Sensitivity::Public, Complexity::Easy)
        };
        let d = engine().decide(&profile, today());
        assert_eq!(
            routed(&d),
            (
                "provider-b".into(),
                cell(Row::ProviderStandard, Column::Top)
            )
        );
        assert!(
            d.explanation
                .reasons()
                .contains(&Reason::ColumnRequested(Column::Top))
        );
    }

    /// The test accounts with one account's status and review date changed.
    fn with_status(
        id: &str,
        status: AccountStatus,
        review_due: Option<&str>,
    ) -> Vec<ProviderAccount> {
        accounts()
            .into_iter()
            .map(|mut a| {
                if a.id.as_str() == id {
                    a.status = status;
                    a.review_due = review_due.map(|d| d.parse().unwrap());
                }
                a
            })
            .collect()
    }

    #[test]
    fn an_overdue_provider_counts_one_row_more_exposed() {
        let mut accounts = with_status("provider-a", AccountStatus::Active, Some("2026-11-01"));
        accounts.retain(|a| a.id.as_str() != "provider-b");
        let d = build(Policy::default(), accounts)
            .decide(&rated(Sensitivity::Public, Complexity::Hard), today());
        assert_eq!(
            routed(&d),
            (
                "provider-a".into(),
                cell(Row::ProviderStandard, Column::Top)
            )
        );
        insta::assert_snapshot!(render(&d));
    }

    #[test]
    fn a_demoted_provider_drops_out_of_a_tighter_ceiling() {
        // Personal may reach no-training terms; demoted, provider-a counts as standard.
        let accounts = with_status("provider-a", AccountStatus::Demoted, None);
        let d = build(Policy::default(), accounts)
            .decide(&rated(Sensitivity::Personal, Complexity::Hard), today());
        assert!(matches!(
            d.outcome,
            Outcome::AskUser(Question::SettleLocal { .. })
        ));
    }

    #[test]
    fn a_review_due_today_is_not_yet_overdue() {
        let accounts = with_status("provider-a", AccountStatus::Active, Some("2026-11-15"));
        let d = build(Policy::default(), accounts)
            .decide(&rated(Sensitivity::Personal, Complexity::Hard), today());
        assert_eq!(
            routed(&d),
            (
                "provider-a".into(),
                cell(Row::ProviderNoTraining, Column::Top)
            )
        );
    }

    #[test]
    fn an_account_marked_out_is_never_used() {
        let accounts = with_status("provider-b", AccountStatus::Out, None);
        let d = build(Policy::default(), accounts)
            .decide(&rated(Sensitivity::Public, Complexity::Hard), today());
        assert_eq!(routed(&d).0, "provider-a");
    }

    #[test]
    fn the_ledger_skips_a_provider_past_its_threshold() {
        let ledger = Ledger::new(3).with("provider-a".parse().unwrap(), Category::General, 5);
        let d = engine()
            .with_ledger(ledger)
            .decide(&rated(Sensitivity::Personal, Complexity::Hard), today());
        assert!(matches!(
            d.outcome,
            Outcome::AskUser(Question::SettleLocal { .. })
        ));
        insta::assert_snapshot!(render(&d));
    }

    #[test]
    fn without_a_working_ledger_slot_nothing_is_skipped() {
        let ledger = Ledger::new(3).with("provider-a".parse().unwrap(), Category::General, 5);
        let d = without(Slot::ExposureLedger)
            .with_ledger(ledger)
            .decide(&rated(Sensitivity::Personal, Complexity::Hard), today());
        assert_eq!(routed(&d).0, "provider-a");
    }
}
