//! The rules that must hold for any policy, any accounts and any request (TODO §1.5).
//!
//! Each property restates the rule independently of the engine's code, then checks the engine
//! against it for many generated inputs.

use bullbar_core::capabilities::{Capabilities, Capability};
use bullbar_core::decision::{Mechanism, Outcome, Question};
use bullbar_core::engine::Engine;
use bullbar_core::ledger::Ledger;
use bullbar_core::levels::{Category, Column, Complexity, Row, ScrubberGrade, Sensitivity};
use bullbar_core::lifecycle::{Attempt, Event, Judge, Lifecycle, State};
use bullbar_core::policy::{
    AdminPolicy, CategoryRule, ColumnMinimums, Escalation, Policy, RowCeilings, UserPolicy,
};
use bullbar_core::profile::RequestProfile;
use bullbar_core::provider::{AccountId, AccountStatus, Date, Offer, ProviderAccount};
use bullbar_core::slots::{Slot, SlotState, Slots};
use proptest::prelude::*;
use proptest::sample::select;
use std::collections::BTreeMap;

fn row() -> impl Strategy<Value = Row> {
    select(Row::ALL)
}

fn column() -> impl Strategy<Value = Column> {
    select(Column::ALL)
}

/// A capability cap, mostly Top: the user's cap is the lesser of two, and a low cap leaves a
/// poor answer nothing more capable to try.
fn cap() -> impl Strategy<Value = Column> {
    prop_oneof![1 => Just(Column::Small), 1 => Just(Column::Mid), 4 => Just(Column::Top)]
}

fn sensitivity() -> impl Strategy<Value = Sensitivity> {
    select(Sensitivity::ALL)
}

/// True four times in five.
fn often() -> impl Strategy<Value = bool> {
    prop::bool::weighted(0.8)
}

fn ceilings() -> impl Strategy<Value = RowCeilings> {
    (row(), row(), row()).prop_map(|(public, personal, sensitive)| RowCeilings {
        public,
        personal,
        sensitive,
    })
}

fn categories() -> impl Strategy<Value = BTreeMap<Category, (Row, bool)>> {
    prop::collection::btree_map(select(Category::BUILT_IN), (row(), any::<bool>()), 0..3)
}

/// An admin policy, and user values generated within it, so every policy is valid.
fn policy() -> impl Strategy<Value = Policy> {
    let admin = (
        ceilings(),
        categories(),
        cap(),
        (column(), column(), column()),
        // Switches mostly on, so the paths they open get exercised.
        (often(), often(), often(), often()),
    )
        .prop_map(
            |(
                ceilings,
                categories,
                max_column,
                (easy, medium, hard),
                (strip, exceptions, lower, sensitive_out),
            )| {
                AdminPolicy {
                    ceilings,
                    categories: categories
                        .into_iter()
                        .map(|(c, (ceiling, locked))| (c, CategoryRule { ceiling, locked }))
                        .collect(),
                    max_column,
                    minimum_columns: ColumnMinimums { easy, medium, hard },
                    strip_and_send_allowed: strip,
                    exceptions_allowed: exceptions,
                    lower_ratings_allowed: lower,
                    send_sensitive_without_local_allowed: sensitive_out,
                    ..AdminPolicy::default()
                }
            },
        );
    let escalation = select(vec![
        Escalation::Auto,
        Escalation::AskBeforeLeavingLocal,
        Escalation::Ask,
        Escalation::Off,
    ]);
    let user = (
        ceilings(),
        categories(),
        cap(),
        any::<bool>(),
        often(),
        escalation,
        often(),
        any::<bool>(),
    );
    (admin, user).prop_map(
        |(
            admin,
            (
                ceilings,
                categories,
                max_column,
                strip,
                exceptions,
                escalation,
                sensitive_out,
                chaff_out,
            ),
        )| {
            let within = |rating, row: Row| row.min(admin.ceilings.for_rating(rating));
            let user = UserPolicy {
                ceilings: RowCeilings {
                    public: within(Sensitivity::Public, ceilings.public),
                    personal: within(Sensitivity::Personal, ceilings.personal),
                    sensitive: within(Sensitivity::Sensitive, ceilings.sensitive),
                },
                categories: categories
                    .into_iter()
                    .map(|(c, (row, _))| {
                        let limit = admin
                            .categories
                            .get(&c)
                            .map_or(Row::ProviderStandard, |r| r.ceiling);
                        (c, row.min(limit))
                    })
                    .collect(),
                max_column: max_column.min(admin.max_column),
                strip_and_send: strip && admin.strip_and_send_allowed,
                exceptions: exceptions && admin.exceptions_allowed,
                escalation,
                send_sensitive_without_local: sensitive_out
                    && admin.send_sensitive_without_local_allowed,
                chaff_without_local: chaff_out,
            };
            Policy::new(admin, user).expect("generated within the admin's limits")
        },
    )
}

/// The day every generated case is decided on.
fn today() -> Date {
    date("2026-11-15")
}

fn date(s: &str) -> Date {
    s.parse().expect("a valid date")
}

/// Accounts mostly active, some demoted or out; review dates before, on and after today.
fn accounts() -> impl Strategy<Value = Vec<ProviderAccount>> {
    let offer = (column(), 0u32..20).prop_map(|(column, cost)| Offer { column, cost });
    let status = prop_oneof![
        6 => Just(AccountStatus::Active),
        1 => Just(AccountStatus::Demoted),
        1 => Just(AccountStatus::Out),
    ];
    let due = prop::option::of(select(vec![
        date("2026-11-14"),
        date("2026-11-15"),
        date("2026-11-16"),
    ]));
    let account = (row(), prop::collection::vec(offer, 1..4), status, due);
    prop::collection::vec(account, 1..6).prop_map(|accounts| {
        accounts
            .into_iter()
            .enumerate()
            .map(|(i, (row, offers, status, review_due))| ProviderAccount {
                status,
                review_due,
                ..ProviderAccount::new(
                    format!("account-{i}")
                        .parse()
                        .expect("generated ids are valid"),
                    row,
                    offers,
                )
            })
            .collect()
    })
}

/// The random accounts plus a full grid (one active account per row, every column; costs rise
/// with capability, as they do in use), so a lifecycle usually has a next cell to try.
fn rich_accounts() -> impl Strategy<Value = Vec<ProviderAccount>> {
    let costs = prop::collection::vec(0u32..20, Row::ALL.len() * Column::ALL.len());
    (accounts(), costs).prop_map(|(mut accounts, costs)| {
        let mut costs = costs.into_iter();
        for &row in Row::ALL {
            let offers = Column::ALL
                .iter()
                .zip([0, 20, 40])
                .map(|(&column, step)| Offer {
                    column,
                    cost: step + costs.next().unwrap_or(0),
                })
                .collect();
            let id = format!("grid-{row}")
                .parse()
                .expect("generated ids are valid");
            accounts.push(ProviderAccount::new(id, row, offers));
        }
        accounts
    })
}

/// The row an account counts as today with every slot healthy, or `None` when it is out. A cloud
/// account demoted or past its review date counts one row more exposed; local ones have no terms.
fn row_today(account: &ProviderAccount) -> Option<Row> {
    let unreviewed = account.status == AccountStatus::Demoted
        || account.review_due.is_some_and(|due| due < today());
    match account.status {
        AccountStatus::Out => None,
        _ if account.row == Row::Local => Some(Row::Local),
        _ if unreviewed => Some(account.row.next().unwrap_or(account.row)),
        _ => Some(account.row),
    }
}

/// A ledger over the generated accounts' ids, past its threshold for some of them.
fn ledger() -> impl Strategy<Value = Ledger> {
    let entry = (0usize..6, select(Category::BUILT_IN), 0u32..6);
    prop::collection::vec(entry, 0..8).prop_map(|entries| {
        entries
            .into_iter()
            .fold(Ledger::new(3), |ledger, (i, category, count)| {
                let id = format!("account-{i}")
                    .parse()
                    .expect("generated ids are valid");
                ledger.with(id, category, count)
            })
    })
}

fn profile() -> impl Strategy<Value = RequestProfile> {
    let ratings = (
        prop::option::of(sensitivity()),
        prop::option::of(sensitivity()),
    );
    let request = (
        select(Complexity::ALL),
        select(Category::BUILT_IN),
        prop::bool::weighted(0.2), // credentials found
    );
    // Request limits rare, as in use: half the time would leave lifecycles little room.
    let overrides = (
        prop::option::weighted(0.2, row()),
        prop::option::of(column()),
        prop::bool::weighted(0.1), // shutter closed
    );
    // A pin now and then, to a generated account or to one that isn't set up.
    let pinned = prop::option::weighted(
        0.3,
        (0usize..5).prop_map(|i| {
            format!("account-{}", if i == 4 { 9 } else { i })
                .parse::<AccountId>()
                .expect("generated ids are valid")
        }),
    );
    (ratings, request, overrides, pinned).prop_map(
        |(
            (classifier_rating, user_rating),
            (complexity, category, credentials_found),
            (row_limit, wanted_column, shutter_closed),
            pinned_to,
        )| RequestProfile {
            classifier_rating,
            user_rating,
            complexity,
            category,
            credentials_found,
            row_limit,
            wanted_column,
            shutter_closed,
            pinned_to,
        },
    )
}

/// An attempt part-way through a lifecycle: confirmations, rungs, and now and then floors.
fn attempt() -> impl Strategy<Value = Attempt> {
    let column_floor = prop::option::weighted(0.2, column());
    let row_floor = prop::option::weighted(0.2, row());
    (any::<bool>(), 0u8..4, column_floor, row_floor).prop_map(
        |(lower_rating_confirmed, rungs, column_floor, row_floor)| Attempt {
            lower_rating_confirmed,
            rungs,
            column_floor,
            row_floor,
        },
    )
}

/// Required slots healthy; each optional slot healthy, empty or unhealthy at random.
fn slots() -> impl Strategy<Value = Slots> {
    let optional: Vec<Slot> = Slot::ALL
        .iter()
        .copied()
        .filter(|s| !s.is_required())
        .collect();
    // Mostly healthy, so most requests can route and the route properties get exercised.
    let states = prop_oneof![
        6 => Just(SlotState::Healthy),
        1 => Just(SlotState::Empty),
        1 => Just(SlotState::Unhealthy),
    ];
    // Mostly a grade that fills the slot; sometimes patterns only, or none stated.
    let grade = prop_oneof![
        3 => Just(Some(ScrubberGrade::Context)),
        3 => Just(Some(ScrubberGrade::Entities)),
        1 => Just(Some(ScrubberGrade::Patterns)),
        1 => Just(None),
    ];
    (prop::collection::vec(states, optional.len()), grade).prop_map(move |(states, grade)| {
        let base = match grade {
            Some(grade) => Slots::required_only().with_scrubber_grade(grade),
            None => Slots::required_only(),
        };
        optional
            .iter()
            .zip(states)
            .fold(base, |slots, (&slot, state)| slots.with(slot, state))
    })
}

/// What the gateway might feed a lifecycle, weighted toward poor answers and the moves after.
fn event() -> impl Strategy<Value = Event> {
    let answered = |judged_by, good| Just(Event::Answered { judged_by, good });
    prop_oneof![
        4 => answered(Judge::Screener, false),
        4 => answered(Judge::User, false),
        1 => answered(Judge::Screener, true),
        1 => answered(Judge::User, true),
        2 => Just(Event::UserConfirmsLowerRating),
        2 => Just(Event::UserSettles),
        3 => Just(Event::UserApprovesEscalation),
        1 => Just(Event::UserRefuses),
        3 => Just(Event::UserAsksMoreCapable),
        3 => Just(Event::UserAsksOneRung),
        1 => Just(Event::UserAccepts),
        1 => Just(Event::UserStops),
    ]
}

/// A step of a generated lifecycle: nine times in ten an event that fits the state (picked by
/// `pick`), otherwise `raw` as it comes, so events that don't fit get tested too.
type Step = (u8, usize, Event);

fn steps() -> impl Strategy<Value = Vec<Step>> {
    prop::collection::vec((0u8..10, any::<usize>(), event()), 0..16)
}

fn next_event(state: &State, (odd, pick, raw): Step) -> Event {
    let answered = |judged_by, good| Event::Answered { judged_by, good };
    let fitting = match state {
        State::AwaitingUser(Question::ConfirmLowerRating { .. }) => {
            vec![Event::UserConfirmsLowerRating, Event::UserRefuses]
        }
        State::AwaitingUser(Question::SettleLocal { .. }) => {
            vec![Event::UserSettles, Event::UserRefuses]
        }
        State::AwaitingUser(Question::ApproveEscalation { .. }) => {
            vec![Event::UserApprovesEscalation, Event::UserRefuses]
        }
        State::AwaitingUser(Question::MovePin { .. }) => {
            vec![Event::UserMovesPin, Event::UserRefuses]
        }
        State::Sent(_) => vec![
            answered(Judge::Screener, false),
            answered(Judge::Screener, false),
            answered(Judge::User, false),
            answered(Judge::User, false),
            answered(Judge::Screener, true),
        ],
        State::Poor(_) => vec![
            Event::UserAsksMoreCapable,
            Event::UserAsksOneRung,
            Event::UserAsksOneRung,
            Event::UserAccepts,
        ],
        State::Done(_) | State::Refused | State::Stopped => vec![],
    };
    if odd == 0 || fitting.is_empty() {
        raw
    } else {
        fitting[pick % fitting.len()]
    }
}

fn engine(policy: &Policy, accounts: &[ProviderAccount], slots: Slots) -> Engine {
    Engine::new(policy.clone(), accounts.to_vec(), slots).expect("required slots are healthy")
}

/// The ceiling before any request overrides: the user's ceiling for the rating, tightened by any
/// category ceiling; Local for Secret or credentials left in.
fn base_ceiling(policy: &Policy, p: &RequestProfile, rating: Sensitivity) -> Row {
    if rating == Sensitivity::Secret || (p.credentials_found && !policy.user().strip_and_send) {
        return Row::Local;
    }
    let user = policy.user();
    let admin_category = policy
        .admin()
        .categories
        .get(&p.category)
        .map(|r| r.ceiling);
    let user_category = user.categories.get(&p.category).copied();
    [admin_category, user_category]
        .into_iter()
        .flatten()
        .fold(user.ceilings.for_rating(rating), Row::min)
}

/// The ceiling the rules give with every slot healthy: the base ceiling, raised by exception
/// rungs where allowed (one row each, never past the admin's limit), then held to the request's
/// own limit, and to Local while the shutter is closed.
fn expected_ceiling(policy: &Policy, p: &RequestProfile, a: &Attempt, rating: Sensitivity) -> Row {
    let base = base_ceiling(policy, p, rating);
    let local_only =
        rating == Sensitivity::Secret || (p.credentials_found && !policy.user().strip_and_send);
    let locked = policy
        .admin()
        .categories
        .get(&p.category)
        .is_some_and(|r| r.locked);
    let allowed =
        !local_only && !locked && policy.admin().exceptions_allowed && policy.user().exceptions;
    let mut ceiling = base;
    if allowed {
        for _ in 0..a.rungs {
            ceiling = ceiling.next().unwrap_or(ceiling);
        }
        ceiling = ceiling.min(admin_ceiling(policy, p, rating)).max(base);
    }
    let ceiling = p.row_limit.map_or(ceiling, |limit| ceiling.min(limit));
    if p.shutter_closed {
        Row::Local
    } else {
        ceiling
    }
}

/// The admin's hard limit for a rating and category, whatever the user or the request says.
fn admin_ceiling(policy: &Policy, p: &RequestProfile, rating: Sensitivity) -> Row {
    let admin = policy.admin();
    let category = admin.categories.get(&p.category).map(|r| r.ceiling);
    category
        .into_iter()
        .fold(admin.ceilings.for_rating(rating), Row::min)
}

/// The rating the rules say applies, or `None` when the user must confirm a lower rating first.
/// Lowering below the classifier is ignored where the admin forbids it or the category is locked.
fn expected_rating(policy: &Policy, p: &RequestProfile, a: &Attempt) -> Option<Sensitivity> {
    let may_lower = policy.admin().lower_ratings_allowed
        && !policy
            .admin()
            .categories
            .get(&p.category)
            .is_some_and(|r| r.locked);
    match (p.user_rating, p.classifier_rating) {
        (Some(user), Some(classifier)) if user < classifier && !may_lower => Some(classifier),
        (Some(user), Some(classifier)) if user < classifier && !a.lower_rating_confirmed => None,
        (Some(user), _) => Some(user),
        (None, Some(classifier)) => Some(classifier),
        (None, None) => Some(Sensitivity::Sensitive),
    }
}

proptest! {
    /// Secret-rated content, and anything still holding credentials, never leaves Local.
    #[test]
    fn secret_and_credentials_never_leave_local(
        policy in policy(), accounts in accounts(), slots in slots(), p in profile(), a in attempt()
    ) {
        let decision = engine(&policy, &accounts, slots).decide_attempt(&p, &a, today());
        let local_only = expected_rating(&policy, &p, &a) == Some(Sensitivity::Secret)
            || (p.credentials_found && !policy.user().strip_and_send);
        if let (true, Outcome::Route(route)) = (local_only, &decision.outcome) {
            prop_assert!(!route.leaves_local(), "left local: {}", decision.outcome);
        }
    }

    /// A route lands inside both limits, at the cheapest cost on offer there; with nothing on
    /// offer the engine asks or refuses, never routes.
    #[test]
    fn routes_are_the_cheapest_inside_both_limits(
        policy in policy(), accounts in accounts(), p in profile(), a in attempt()
    ) {
        let decision = engine(&policy, &accounts, Slots::all_healthy()).decide_attempt(&p, &a, today());
        let Some(rating) = expected_rating(&policy, &p, &a) else {
            let asks_to_confirm = matches!(
                decision.outcome,
                Outcome::AskUser(Question::ConfirmLowerRating { .. })
            );
            prop_assert!(asks_to_confirm, "got {}", decision.outcome);
            return Ok(());
        };
        let ceiling = expected_ceiling(&policy, &p, &a, rating);
        let cap = policy.user().max_column;
        let mapped = policy.admin().minimum_columns.for_complexity(p.complexity);
        let minimum = p.wanted_column.map_or(mapped, |w| w.max(mapped)).min(cap);
        let minimum = a.column_floor.map_or(minimum, |floor| floor.max(minimum));
        let row_floor = a.row_floor.unwrap_or(Row::Local);
        // With every slot healthy, topic pinning works: personal work that may leave goes only to
        // the pinned account, or stays local (ADR 0021).
        let pin = p
            .pinned_to
            .as_ref()
            .filter(|_| rating >= Sensitivity::Personal && ceiling > Row::Local);
        let cheapest_among = |pinned: Option<&AccountId>| {
            accounts
                .iter()
                .filter(|acc| row_today(acc).is_some_and(|row| row >= row_floor && row <= ceiling))
                .filter(|acc| pinned.is_none_or(|pin| acc.row == Row::Local || acc.id == *pin))
                .flat_map(|acc| acc.offers.iter())
                .filter(|o| o.column >= minimum && o.column <= cap)
                .map(|o| o.cost)
                .min()
        };
        let cheapest = cheapest_among(pin);
        let elsewhere = cheapest_among(None);

        match (&decision.outcome, cheapest) {
            (Outcome::Route(route), Some(cost)) => {
                let cell = route.cell();
                prop_assert!(
                    cell.row >= row_floor && cell.row <= ceiling,
                    "outside the rows: {cell}"
                );
                prop_assert!(
                    cell.column >= minimum && cell.column <= cap,
                    "outside the columns: {cell}"
                );
                // The chosen account must offer this column at the cheapest cost. (Generated
                // accounts may list a column twice; config validation will reject that.)
                let offered_at_cost = accounts
                    .iter()
                    .filter(|acc| acc.id == *route.account() && row_today(acc) == Some(cell.row))
                    .flat_map(|acc| acc.offers.iter())
                    .any(|o| o.column == cell.column && o.cost == cost);
                prop_assert!(offered_at_cost, "not the cheapest: {}", decision.outcome);
            }
            (Outcome::Route(_), None) => {
                prop_assert!(false, "routed with nothing on offer: {}", decision.outcome);
            }
            (_, Some(_)) => {
                prop_assert!(false, "didn't route with a cell on offer: {}", decision.outcome);
            }
            (Outcome::AskUser(Question::SettleLocal { route, .. }), None) => {
                prop_assert!(!route.leaves_local());
            }
            // Pinned elsewhere, and another account could take it: ask before moving.
            (Outcome::AskUser(Question::MovePin { .. }), None) => {
                prop_assert!(pin.is_some() && elsewhere.is_some(), "{}", decision.outcome);
            }
            (Outcome::Refuse, None) => {}
            (Outcome::AskUser(q), None) => prop_assert!(false, "unexpected question: {q}"),
        }
    }

    /// Only scrubbing opens rows: a Personal or Sensitive request that leaves Local is scrubbed,
    /// and stripped credentials always travel with the strip mechanism.
    #[test]
    fn whatever_leaves_is_protected_for_its_rating(
        policy in policy(), accounts in accounts(), slots in slots(), p in profile(), a in attempt()
    ) {
        let decision = engine(&policy, &accounts, slots).decide_attempt(&p, &a, today());
        if let (Outcome::Route(route), Some(rating)) = (&decision.outcome, expected_rating(&policy, &p, &a)) {
            let m = route.mechanisms();
            if !route.leaves_local() {
                prop_assert!(m.is_empty(), "local answer with protections: {}", decision.outcome);
            } else {
                if rating >= Sensitivity::Personal {
                    prop_assert!(m.contains(&Mechanism::Scrub), "unscrubbed: {}", decision.outcome);
                }
                if p.credentials_found {
                    prop_assert!(
                        m.contains(&Mechanism::StripCredentials),
                        "credentials not stripped: {}",
                        decision.outcome
                    );
                }
            }
        }
    }

    /// A closed shutter keeps everything on the local layer, whatever else the request or the
    /// policy says, retries and exceptions included.
    #[test]
    fn a_closed_shutter_lets_nothing_leave(
        policy in policy(), accounts in accounts(), slots in slots(), p in profile(), a in attempt()
    ) {
        let p = RequestProfile { shutter_closed: true, ..p };
        let decision = engine(&policy, &accounts, slots).decide_attempt(&p, &a, today());
        let leaves = |route: &bullbar_core::decision::Route| route.leaves_local();
        match &decision.outcome {
            Outcome::Route(route) => prop_assert!(!leaves(route), "left: {}", decision.outcome),
            Outcome::AskUser(Question::SettleLocal { route, .. }
                | Question::ApproveEscalation { route }
                | Question::MovePin { route, .. }) => {
                prop_assert!(!leaves(route), "offered: {}", decision.outcome);
            }
            Outcome::AskUser(Question::ConfirmLowerRating { .. }) | Outcome::Refuse => {}
        }
    }

    /// A pinned topic's work never leaves for another account without asking (ADR 0021): a route
    /// with topic pinning engaged goes to the pinned account, and a question to move the pin offers
    /// a different one.
    #[test]
    fn a_pinned_topic_never_moves_without_asking(
        policy in policy(), accounts in accounts(), slots in slots(), p in profile(), a in attempt()
    ) {
        let decision = engine(&policy, &accounts, slots).decide_attempt(&p, &a, today());
        match (&decision.outcome, &p.pinned_to) {
            (Outcome::Route(route), Some(pinned))
                if route.mechanisms().contains(&Mechanism::TopicPinning) =>
            {
                prop_assert_eq!(route.account(), pinned, "{}", decision.outcome);
            }
            (Outcome::AskUser(Question::MovePin { from, route, .. }), pinned) => {
                prop_assert_eq!(Some(from), pinned.as_ref());
                prop_assert_ne!(route.account(), from);
                prop_assert!(route.leaves_local());
            }
            _ => {}
        }
    }

    /// Credentials are never put back into a reply (ADR 0009): Restore is engaged only for
    /// scrubbed personal details, never for stripped credentials alone.
    #[test]
    fn credentials_are_never_restored(
        policy in policy(), accounts in accounts(), slots in slots(), p in profile(), a in attempt()
    ) {
        let decision = engine(&policy, &accounts, slots).decide_attempt(&p, &a, today());
        if let Outcome::Route(route) = &decision.outcome {
            let m = route.mechanisms();
            if m.contains(&Mechanism::Restore) {
                prop_assert!(m.contains(&Mechanism::Scrub), "restore without scrub: {}", decision.outcome);
            }
        }
    }

    /// A missing optional slot never loosens anything: a route is never more exposed than the
    /// policy's ceiling for its rating, and Personal or Sensitive never leave without a scrubber.
    #[test]
    fn missing_slots_never_loosen(
        policy in policy(), accounts in accounts(), slots in slots(), p in profile(), a in attempt()
    ) {
        let scrubber = slots.usable(Slot::PiiScrubber);
        let decision = engine(&policy, &accounts, slots).decide_attempt(&p, &a, today());
        if let (Outcome::Route(route), Some(rating)) = (&decision.outcome, expected_rating(&policy, &p, &a)) {
            let row = route.cell().row;
            let ceiling = expected_ceiling(&policy, &p, &a, rating);
            prop_assert!(row <= ceiling, "{row} above the ceiling {ceiling}");
            if !scrubber && rating >= Sensitivity::Personal {
                prop_assert!(!route.leaves_local(), "left unscrubbed: {}", decision.outcome);
            }
        }
    }

    /// A missing dependency only removes capabilities, never adds any (ADR 0002, amendment):
    /// emptying any slot leaves a subset of what was there.
    #[test]
    fn a_missing_slot_only_removes_capabilities(slots in slots(), slot in select(Slot::ALL)) {
        let before = Capabilities::new(&slots);
        for state in [SlotState::Empty, SlotState::Unhealthy] {
            let after = Capabilities::new(&slots.clone().with(slot, state));
            for cap in after.working() {
                prop_assert!(before.has(cap), "{cap} appeared when {slot} became {state}");
            }
        }
    }

    /// Personas go out only when they can be undone: an Obfuscator, a Deobfuscator and a working
    /// local runner for persona correction (ADR 0015).
    #[test]
    fn personas_leave_only_with_their_undo(
        policy in policy(), accounts in accounts(), slots in slots(), p in profile(), a in attempt()
    ) {
        let undo = [Slot::Obfuscator, Slot::Deobfuscator, Slot::LocalRunner]
            .iter()
            .all(|&s| slots.usable(s));
        let decision = engine(&policy, &accounts, slots).decide_attempt(&p, &a, today());
        if let Outcome::Route(route) = &decision.outcome {
            prop_assert!(
                undo || !route.mechanisms().contains(&Mechanism::Personas),
                "personas without their undo: {}",
                decision.outcome
            );
        }
    }

    /// Decoy content goes out only when it can be stripped from the reply, and chaff without a
    /// local runner only when the user asked for canned chaff (ADR 0015).
    #[test]
    fn decoys_and_chaff_leave_only_where_allowed(
        policy in policy(), accounts in accounts(), slots in slots(), p in profile(), a in attempt()
    ) {
        let runner = slots.usable(Slot::LocalRunner);
        let undo = runner && slots.usable(Slot::Obfuscator) && slots.usable(Slot::Deobfuscator);
        let decision = engine(&policy, &accounts, slots).decide_attempt(&p, &a, today());
        if let Outcome::Route(route) = &decision.outcome {
            let m = route.mechanisms();
            prop_assert!(undo || !m.contains(&Mechanism::DecoyContent), "{}", decision.outcome);
            prop_assert!(
                runner || policy.user().chaff_without_local || !m.contains(&Mechanism::Chaff),
                "canned chaff not asked for: {}",
                decision.outcome
            );
        }
    }

    /// With no local model, a Sensitive request leaves only scrubbed, and never where the user or
    /// the admin has turned that off (ADR 0014).
    #[test]
    fn sensitive_without_a_local_model_leaves_only_scrubbed_and_only_where_allowed(
        policy in policy(), accounts in accounts(), slots in slots(), p in profile(), a in attempt()
    ) {
        let no_runner = !slots.usable(Slot::LocalRunner);
        let decision = engine(&policy, &accounts, slots).decide_attempt(&p, &a, today());
        if let (true, Some(Sensitivity::Sensitive), Outcome::Route(route)) =
            (no_runner, expected_rating(&policy, &p, &a), &decision.outcome)
        {
            prop_assert!(policy.user().send_sensitive_without_local, "{}", decision.outcome);
            prop_assert!(route.mechanisms().contains(&Mechanism::Scrub), "{}", decision.outcome);
        }
    }

    /// Nothing ever routes past the admin's hard limits, whatever the user's values or the
    /// slots: not more exposed than the admin's ceiling for the rating and category, not more
    /// capable than the admin's cap.
    #[test]
    fn nothing_passes_the_admins_limits(
        policy in policy(), accounts in accounts(), slots in slots(), p in profile(), a in attempt()
    ) {
        let decision = engine(&policy, &accounts, slots).decide_attempt(&p, &a, today());
        let routes = match &decision.outcome {
            Outcome::Route(route) => vec![route],
            Outcome::AskUser(Question::SettleLocal { route, .. }) => vec![route],
            _ => vec![],
        };
        if let Some(rating) = expected_rating(&policy, &p, &a) {
            for route in routes {
                let cell = route.cell();
                let limit = admin_ceiling(&policy, &p, rating);
                prop_assert!(cell.row <= limit, "{} past the admin's {limit}", cell.row);
                prop_assert!(cell.column <= policy.admin().max_column, "{} past the cap", cell.column);
            }
        }
    }

    /// A request's own limit always holds, whatever exceptions it also asks for.
    #[test]
    fn a_requests_own_limit_always_holds(
        policy in policy(), accounts in accounts(), slots in slots(), p in profile(), a in attempt()
    ) {
        let decision = engine(&policy, &accounts, slots).decide_attempt(&p, &a, today());
        if let (Outcome::Route(route), Some(limit)) = (&decision.outcome, p.row_limit) {
            prop_assert!(route.cell().row <= limit, "past the request's {limit}: {}", decision.outcome);
        }
    }

    /// The engine never loosens by itself: without exception rungs, nothing goes past the base
    /// ceiling; each rung adds at most one row.
    #[test]
    fn exceptions_add_at_most_one_row_per_rung(
        policy in policy(), accounts in accounts(), slots in slots(), p in profile(), a in attempt()
    ) {
        let decision = engine(&policy, &accounts, slots).decide_attempt(&p, &a, today());
        if let (Outcome::Route(route), Some(rating)) =
            (&decision.outcome, expected_rating(&policy, &p, &a))
        {
            let base = base_ceiling(&policy, &p, rating);
            let bound = (0..a.rungs).fold(base, |r, _| r.next().unwrap_or(r));
            prop_assert!(route.cell().row <= bound, "{} past {bound}", route.cell().row);
        }
    }

    /// Unreviewed provider changes demote, never promote: a route lands on the row its account
    /// counts as today, never less exposed than its reviewed row, and never on an account that is
    /// out.
    #[test]
    fn unreviewed_changes_demote_never_promote(
        policy in policy(), accounts in accounts(), p in profile(), a in attempt()
    ) {
        let decision = engine(&policy, &accounts, Slots::all_healthy()).decide_attempt(&p, &a, today());
        if let Outcome::Route(route) = &decision.outcome {
            let account = accounts.iter().find(|acc| acc.id == *route.account());
            let account = account.expect("routes go to a known account");
            let row = route.cell().row;
            prop_assert_eq!(Some(row), row_today(account), "{}", decision.outcome);
            prop_assert!(row >= account.row, "promoted: {}", decision.outcome);
        }
    }

    /// With a working exposure ledger, a request never goes to a cloud account that has seen
    /// enough of its category.
    #[test]
    fn the_ledger_skips_accounts_past_its_threshold(
        policy in policy(), accounts in accounts(), ledger in ledger(), p in profile(), a in attempt()
    ) {
        let engine = engine(&policy, &accounts, Slots::all_healthy()).with_ledger(ledger.clone());
        let decision = engine.decide_attempt(&p, &a, today());
        if let Outcome::Route(route) = &decision.outcome
            && route.leaves_local()
        {
            let seen = ledger.exceeded(route.account(), &p.category);
            prop_assert!(seen.is_none(), "past the threshold: {}", decision.outcome);
        }
    }

    /// Every decision explains itself.
    #[test]
    fn every_decision_has_an_explanation(
        policy in policy(), accounts in accounts(), slots in slots(), p in profile(), a in attempt()
    ) {
        let decision = engine(&policy, &accounts, slots).decide_attempt(&p, &a, today());
        prop_assert!(!decision.explanation.reasons().is_empty());
    }
}

proptest! {
    // About one generated lifecycle in eight sends more than once (see the probe), so this
    // property runs four times the default cases.
    #![proptest_config(ProptestConfig::with_cases(1024))]

    /// Whole lifecycles, driven by random events (ADR 0013): an event that doesn't fit changes
    /// nothing; the answer check never takes a rung; every attempt lands on a new cell, strictly
    /// forward on (column, then row); and every route sent keeps the rules above.
    #[test]
    fn lifecycles_move_forward_and_keep_every_rule(
        policy in policy(),
        accounts in rich_accounts(),
        slots in slots(),
        p in profile(),
        steps in steps(),
    ) {
        let engine = engine(&policy, &accounts, slots);
        let mut life = Lifecycle::start(&engine, p, today());
        for step in steps {
            let event = next_event(life.state(), step);
            let before = life.clone();
            match life.handle(&engine, event) {
                Err(_) => prop_assert_eq!(&life, &before),
                Ok(()) => {
                    if let Event::Answered { judged_by: Judge::Screener, .. } = event {
                        prop_assert_eq!(life.attempt().rungs, before.attempt().rungs);
                    }
                }
            }
            let cells: Vec<_> = life
                .sent()
                .iter()
                .map(|route| (route.cell().column, route.cell().row))
                .collect();
            prop_assert!(cells.windows(2).all(|w| w[0] < w[1]), "not forward: {:?}", cells);
            prop_assert!(cells.len() <= Row::ALL.len() * Column::ALL.len());

            let (p, a) = (life.profile(), life.attempt());
            let Some(rating) = expected_rating(&policy, p, a) else {
                prop_assert!(life.sent().is_empty());
                continue;
            };
            let local_only = rating == Sensitivity::Secret
                || (p.credentials_found && !policy.user().strip_and_send);
            let bound = (0..a.rungs).fold(base_ceiling(&policy, p, rating), |r, _| r.up(1));
            for route in life.sent() {
                let row = route.cell().row;
                prop_assert!(!(local_only && route.leaves_local()), "left local: {}", route);
                prop_assert!(row <= admin_ceiling(&policy, p, rating), "past the admin: {}", route);
                prop_assert!(row <= bound, "past {} after {} rungs: {}", bound, a.rungs, route);
            }
            if let State::Sent(route) = life.state() {
                prop_assert_eq!(Some(route), life.sent().last());
            }
        }
    }
}

/// How often the generated cases reach the interesting paths. The properties above only check
/// routes, so if few cases route they check little: on 2026-11-15 this showed 164 of 2000 routing,
/// and the generators were reweighted. Run after changing a generator:
/// `cargo nextest run --run-ignored only probe_coverage --no-capture`.
#[test]
#[ignore = "coverage probe, run by hand"]
fn probe_coverage() {
    use bullbar_core::decision::Reason;
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;
    let mut runner = TestRunner::default();
    let (mut taken, mut limited, mut blocked, mut routed) = (0, 0, 0, 0);
    for _ in 0..2000 {
        let (policy, accounts, slots, p, a) = (policy(), accounts(), slots(), profile(), attempt())
            .new_tree(&mut runner)
            .unwrap()
            .current();
        let d = engine(&policy, &accounts, slots).decide_attempt(&p, &a, today());
        if matches!(d.outcome, Outcome::Route(_)) {
            routed += 1;
        }
        for r in d.explanation.reasons() {
            match r {
                Reason::ExceptionTaken { .. } => taken += 1,
                Reason::RequestLimit(_) => limited += 1,
                Reason::ExceptionNotAllowed(_) => blocked += 1,
                _ => {}
            }
        }
    }
    println!(
        "routed {routed}, exception taken {taken}, blocked {blocked}, request limit {limited}"
    );

    // Lifecycles: how many send more than once, take a rung, or stop to ask about escalating.
    let (mut retried, mut rung, mut asked, mut longest) = (0, 0, 0, 0);
    let (mut started, mut judged_poor) = (0, 0);
    for _ in 0..2000 {
        let (policy, accounts, slots, p, steps) =
            (policy(), rich_accounts(), slots(), profile(), steps())
                .new_tree(&mut runner)
                .unwrap()
                .current();
        let engine = engine(&policy, &accounts, slots);
        let mut life = Lifecycle::start(&engine, p, today());
        let mut was_asked = false;
        let mut was_poor = false;
        started += usize::from(matches!(life.state(), State::Sent(_)));
        for step in steps {
            let _ = life.handle(&engine, next_event(life.state(), step));
            was_poor |= matches!(life.state(), State::Poor(_));
            was_asked |= matches!(
                life.state(),
                State::AwaitingUser(Question::ApproveEscalation { .. })
            );
        }
        retried += usize::from(life.sent().len() > 1);
        rung += usize::from(life.attempt().rungs > 0);
        asked += usize::from(was_asked);
        judged_poor += usize::from(was_poor);
        longest = longest.max(life.sent().len());
    }
    println!("lifecycles: sent at once {started}, judged poor {judged_poor}");
    println!(
        "lifecycles: sent again {retried}, took a rung {rung}, asked to escalate {asked}, longest {longest}"
    );
}

/// The outbound check never depends on the scrubber: its value is catching what the scrubber
/// misses (ADR 0002, amendment).
#[test]
fn the_outbound_check_never_depends_on_the_scrubber() {
    let needed = Capability::CheckOutbound.slots_needed();
    assert!(!needed.contains(&Slot::PiiScrubber), "{needed:?}");
    let caps = Capabilities::new(&Slots::required_only());
    assert!(caps.has(Capability::CheckOutbound));
}

/// Required slots never depend on optional ones, or an empty optional slot would quietly break a
/// required one (ADR 0002, amendment).
#[test]
fn required_slots_never_depend_on_optional_ones() {
    for &cap in Capability::ALL {
        if cap.spec().provided_by.is_required() {
            for slot in cap.slots_needed() {
                assert!(
                    slot.is_required(),
                    "{cap} (required) needs {slot} (optional)"
                );
            }
        }
    }
}
