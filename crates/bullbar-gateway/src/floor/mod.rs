//! The always-on floor (design rules 4 and 5): the credential scanner finds credentials in every
//! request, and the outbound check looks once more at the exact body about to leave. They share no
//! rules and no code, so a miss in one isn't a miss in the other (ADR 0009: the second guard).
//!
//! Nothing here can be switched off. A hit, or anything the floor can't check, blocks the request.

pub mod outbound;
pub mod scanner;

use std::fmt;

use bullbar_core::decision::{Mechanism, Route};
use serde_json::Value;

use outbound::{Hit, OutboundCheck, Spot};
use scanner::Scanner;

use crate::scrub::{Replaced, ScrubError};

/// What a stripped credential is replaced with: `[CREDENTIAL_1]`, `[CREDENTIAL_2]`, ... numbered
/// per request, the same value always getting the same number.
pub fn placeholder(n: usize) -> String {
    format!("[CREDENTIAL_{n}]")
}

/// Why the floor stopped a request. Never carries the credential itself.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FloorError {
    /// Credentials found, and the route doesn't strip them. The engine never decides this; the
    /// floor checks anyway.
    #[error("the request still holds credentials and this route doesn't strip them")]
    CredentialsLeft,
    /// The outbound check found something in the body about to leave.
    #[error("the outbound check found {0} (at {1}) in the request about to leave")]
    Outbound(Hit, Spot),
    /// The body couldn't be written out to be checked.
    #[error("the request couldn't be written out to be checked")]
    Unwritable,
    /// The route needs scrubbing and no scrubber was given. The gateway never does this; the
    /// floor checks anyway.
    #[error("this route needs scrubbing and there is no scrubber")]
    Unscrubbed,
    /// The scrubber failed, so the conversation wasn't fully checked.
    #[error("the scrubber failed ({0})")]
    Scrubbing(String),
}

/// The scrubbing step a route may need: scrubs the body in place and says what it replaced.
pub type Scrub<'a> = &'a dyn Fn(&mut Value) -> Result<Replaced, ScrubError>;

/// The credentials taken out of a request, in placeholder order, each with what it is. Its debug
/// output hides them.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Stripped(Vec<(String, &'static str)>);

impl Stripped {
    /// How many distinct credentials were taken out.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// True when nothing was taken out.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The credentials themselves, for the outbound check.
    pub(crate) fn values(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(|(v, _)| v.as_str())
    }

    /// One line per placeholder saying what it stands for, never the value:
    /// `[CREDENTIAL_1] stands for a token you sent`.
    pub fn described(&self) -> Vec<String> {
        self.0
            .iter()
            .enumerate()
            .map(|(i, (_, label))| format!("{} stands for {label} you sent", placeholder(i + 1)))
            .collect()
    }
}

impl fmt::Debug for Stripped {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Stripped({} hidden)", self.0.len())
    }
}

/// A request that passed the floor, ready to leave.
#[derive(Debug)]
pub struct Prepared {
    /// The body to send, credentials and personal details replaced.
    pub body: Value,
    /// The credentials taken out.
    pub stripped: Stripped,
    /// The personal details replaced.
    pub replaced: Replaced,
}

/// The scanner and the outbound check, built once at start.
pub struct Floor {
    scanner: Scanner,
    check: OutboundCheck,
}

impl Floor {
    /// Builds both rule sets. An error here stops the gateway starting (fail closed).
    pub fn new() -> Result<Self, regex::Error> {
        Ok(Floor {
            scanner: Scanner::new()?,
            check: OutboundCheck::new()?,
        })
    }

    /// Whether the scanner finds credentials anywhere in the body (ADR 0009's input to the engine).
    pub fn credentials_found(&self, body: &Value) -> bool {
        self.scanner.found(body)
    }

    /// Readies a body for a route that leaves the local layer, with no scrubber: a route that
    /// needs one is refused.
    pub fn prepare(&self, body: Value, route: &Route) -> Result<Prepared, FloorError> {
        self.prepare_with(body, route, None)
    }

    /// Readies a body for a route that leaves the local layer: strips credentials if the route
    /// says so, scrubs personal details if it says so, then runs the outbound check on exactly what
    /// would be sent. Credentials go first, so the scanner sees them whole and they are never
    /// mistaken for details that get restored.
    pub fn prepare_with(
        &self,
        mut body: Value,
        route: &Route,
        scrub: Option<Scrub<'_>>,
    ) -> Result<Prepared, FloorError> {
        let stripped = if route.mechanisms().contains(&Mechanism::StripCredentials) {
            self.scanner.strip(&mut body)
        } else if self.scanner.found(&body) {
            return Err(FloorError::CredentialsLeft);
        } else {
            Stripped::default()
        };
        let replaced = if route.mechanisms().contains(&Mechanism::Scrub) {
            let scrub = scrub.ok_or(FloorError::Unscrubbed)?;
            scrub(&mut body).map_err(|e| FloorError::Scrubbing(e.to_string()))?
        } else {
            Replaced::default()
        };
        let text = serde_json::to_string(&body).map_err(|_| FloorError::Unwritable)?;
        let personal: Vec<&str> = replaced.originals().collect();
        self.check
            .check(&body, &text, &stripped, &personal)
            .map_err(|(hit, spot)| FloorError::Outbound(hit, spot))?;
        Ok(Prepared {
            body,
            stripped,
            replaced,
        })
    }
}

/// Calls `f` on every string in a JSON value, object keys included.
pub(crate) fn each_string(value: &Value, f: &mut impl FnMut(&str)) {
    match value {
        Value::String(s) => f(s),
        Value::Array(items) => items.iter().for_each(|v| each_string(v, f)),
        Value::Object(map) => {
            for (key, v) in map {
                f(key);
                each_string(v, f);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

/// Calls `f` on every string value (not keys) in a JSON value, to change it.
pub(crate) fn each_string_mut(value: &mut Value, f: &mut impl FnMut(&mut String)) {
    match value {
        Value::String(s) => f(s),
        Value::Array(items) => items.iter_mut().for_each(|v| each_string_mut(v, f)),
        Value::Object(map) => map.values_mut().for_each(|v| each_string_mut(v, f)),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

/// Fake credentials both rule sets must catch: one of each kind, obviously fake. Shared by the
/// tests of both, so a gap in either shows.
#[cfg(test)]
pub(crate) const FAKES: &[&str] = &[
    "AKIAFAKEFAKEFAKEFAKE",
    "ghp_FAKEfakeFAKEfakeFAKEfakeFAKEfakeFAKE",
    "sk-FAKEfakeFAKEfakeFAKEfake0123456789",
    "xoxb-0000000000-FAKEFAKEFAKE",
    "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJmYWtlIn0.ZmFrZXNpZ25hdHVyZQ",
    "4111 1111 1111 1111",
    "-----BEGIN PRIVATE KEY-----\nRkFLRSBGQUtFIEZBS0U=\n-----END PRIVATE KEY-----",
];

#[cfg(test)]
mod tests {
    use super::*;
    use bullbar_core::decision::{Decision, Outcome};
    use bullbar_core::engine::Engine;
    use bullbar_core::levels::{Category, Column, Complexity, Row, Sensitivity};
    use bullbar_core::policy::{AdminPolicy, Policy, UserPolicy};
    use bullbar_core::profile::RequestProfile;
    use bullbar_core::provider::{Offer, ProviderAccount};
    use bullbar_core::slots::Slots;
    use proptest::prelude::*;
    use serde_json::json;
    use std::sync::LazyLock;

    static FLOOR: LazyLock<Floor> = LazyLock::new(|| Floor::new().unwrap());
    static ROUTES: LazyLock<(Route, Route)> = LazyLock::new(|| (route(false), route(true)));
    static SCRUB_ROUTE: LazyLock<Route> = LazyLock::new(scrub_route);

    fn floor() -> &'static Floor {
        &FLOOR
    }

    /// A leaving route for a public request, with or without credentials, from the real engine.
    fn route(credentials: bool) -> Route {
        let admin = AdminPolicy::default();
        let user = UserPolicy {
            strip_and_send: true,
            ..UserPolicy::within(&admin)
        };
        let policy = Policy::new(admin, user).unwrap();
        let account = ProviderAccount::new(
            "provider-a".parse().unwrap(),
            Row::ProviderNoTraining,
            vec![Offer {
                column: Column::Small,
                cost: 1,
            }],
        );
        let engine = Engine::new(policy, vec![account], Slots::all_healthy()).unwrap();
        let profile = RequestProfile {
            user_rating: Some(Sensitivity::Public),
            credentials_found: credentials,
            ..RequestProfile::new(Complexity::Easy, Category::General)
        };
        let Decision { outcome, .. } = engine.decide(&profile, "2026-11-16".parse().unwrap());
        match outcome {
            Outcome::Route(route) => route,
            other => panic!("expected a route, got {other:?}"),
        }
    }

    /// A leaving route for a personal request with credentials: scrubbing and stripping both.
    fn scrub_route() -> Route {
        let admin = AdminPolicy::default();
        let user = UserPolicy {
            strip_and_send: true,
            ..UserPolicy::within(&admin)
        };
        let policy = Policy::new(admin, user).unwrap();
        let account = ProviderAccount::new(
            "provider-a".parse().unwrap(),
            Row::ProviderNoTraining,
            vec![Offer {
                column: Column::Small,
                cost: 1,
            }],
        );
        let engine = Engine::new(policy, vec![account], Slots::all_healthy()).unwrap();
        let profile = RequestProfile {
            user_rating: Some(Sensitivity::Personal),
            credentials_found: true,
            ..RequestProfile::new(Complexity::Easy, Category::General)
        };
        match engine
            .decide(&profile, "2026-11-16".parse().unwrap())
            .outcome
        {
            Outcome::Route(route) => route,
            other => panic!("expected a route, got {other:?}"),
        }
    }

    /// Finds the listed names as people.
    struct Names(Vec<String>);

    impl crate::scrub::Detector for Names {
        fn find(&self, text: &str) -> Result<Vec<crate::scrub::Found>, crate::scrub::EntityError> {
            Ok(self
                .0
                .iter()
                .flat_map(|n| {
                    text.match_indices(n.as_str())
                        .map(|(at, n)| at..at + n.len())
                })
                .map(|range| crate::scrub::Found {
                    range,
                    kind: crate::scrub::PiiKind::Person,
                })
                .collect())
        }
    }

    fn scrubbing(names: Vec<String>) -> impl Fn(&mut Value) -> Result<Replaced, ScrubError> {
        let scrubber = crate::scrub::Scrubber::with_detector(Box::new(Names(names))).unwrap();
        let keyed = crate::scrub::surrogate::Keyed::from_secret(&"3c".repeat(32)).unwrap();
        move |body: &mut Value| crate::scrub::scrub_conversation(&scrubber, &keyed, body)
    }

    #[test]
    fn a_route_that_scrubs_is_refused_without_a_scrubber() {
        assert!(SCRUB_ROUTE.mechanisms().contains(&Mechanism::Scrub));
        assert_eq!(
            floor()
                .prepare(ask("Ask Imogen"), &SCRUB_ROUTE)
                .unwrap_err(),
            FloorError::Unscrubbed
        );
    }

    #[test]
    fn credentials_are_stripped_before_scrubbing_and_stay_placeholders() {
        let scrub = scrubbing(vec!["Imogen".into()]);
        let body = ask("Imogen's key is AKIAFAKEFAKEFAKEFAKE, number 07700 900123");
        let prepared = floor()
            .prepare_with(body, &SCRUB_ROUTE, Some(&scrub))
            .unwrap();
        let sent = prepared.body.to_string();
        assert!(sent.contains("[CREDENTIAL_1]"), "{sent}");
        assert!(
            !sent.contains("Imogen") && !sent.contains("AKIA") && !sent.contains("07700"),
            "{sent}"
        );
        assert_eq!(prepared.stripped.len(), 1);
        assert_eq!(prepared.replaced.len(), 2);
    }

    #[test]
    fn a_detail_the_scrubber_cannot_reach_is_caught_on_the_way_out() {
        // In a field the scrubber doesn't touch: the outbound check still knows the value.
        let scrub = scrubbing(vec!["Imogen".into()]);
        let body =
            json!({ "messages": [{ "role": "user", "content": "Ask Imogen" }], "user": "imogen" });
        let err = floor()
            .prepare_with(body, &SCRUB_ROUTE, Some(&scrub))
            .unwrap_err();
        assert!(
            matches!(err, FloorError::Outbound(Hit::Personal, _)),
            "{err}"
        );
        assert!(err.to_string().contains("(at user)"), "{err}");
    }

    #[test]
    fn a_failing_scrubber_blocks() {
        let failing = |_: &mut Value| -> Result<Replaced, ScrubError> { Err(ScrubError::Missing) };
        assert!(matches!(
            floor().prepare_with(ask("Ask Imogen"), &SCRUB_ROUTE, Some(&failing)),
            Err(FloorError::Scrubbing(_))
        ));
    }

    fn ask(text: &str) -> Value {
        json!({ "model": "m", "messages": [{ "role": "user", "content": text }] })
    }

    #[test]
    fn a_clean_request_passes_unchanged() {
        let body = ask("what is six times seven?");
        let prepared = floor().prepare(body.clone(), &route(false)).unwrap();
        assert_eq!(prepared.body, body);
        assert!(prepared.stripped.is_empty());
    }

    #[test]
    fn credentials_are_stripped_and_the_rest_leaves() {
        for fake in FAKES {
            let body = ask(&format!("why does this fail?\n{fake}\nthanks"));
            let prepared = floor().prepare(body, &route(true)).unwrap();
            let sent = prepared.body.to_string();
            assert!(sent.contains("[CREDENTIAL_1]"), "{fake}: {sent}");
            assert!(sent.contains("why does this fail?"), "{fake}");
            assert_eq!(prepared.stripped.len(), 1, "{fake}");
        }
    }

    #[test]
    fn the_same_credential_gets_the_same_number() {
        let body = json!({ "messages": [
            { "role": "user", "content": "AKIAFAKEFAKEFAKEFAKE and 4111 1111 1111 1111" },
            { "role": "user", "content": "again AKIAFAKEFAKEFAKEFAKE" },
        ]});
        let prepared = floor().prepare(body, &route(true)).unwrap();
        assert_eq!(prepared.stripped.len(), 2);
        assert_eq!(
            prepared.body["messages"][1]["content"],
            "again [CREDENTIAL_1]"
        );
    }

    #[test]
    fn a_route_that_does_not_strip_never_carries_credentials() {
        let err = floor()
            .prepare(ask("AKIAFAKEFAKEFAKEFAKE"), &route(false))
            .unwrap_err();
        assert_eq!(err, FloorError::CredentialsLeft);
    }

    #[test]
    fn a_credential_the_scanner_cannot_strip_is_caught_on_the_way_out() {
        // In an object key, where stripping doesn't reach.
        let body = json!({ "messages": [], "AKIAFAKEFAKEFAKEFAKE": 1 });
        assert!(matches!(
            floor().prepare(body, &route(true)),
            Err(FloorError::Outbound(..))
        ));
    }

    #[test]
    fn debug_output_hides_what_was_stripped() {
        let prepared = floor()
            .prepare(ask("AKIAFAKEFAKEFAKEFAKE"), &route(true))
            .unwrap();
        let shown = format!("{prepared:?}");
        assert!(!shown.contains("AKIAFAKE"), "{shown}");
    }

    /// Fake credentials of each kind, with random bodies.
    fn fake_credential() -> impl Strategy<Value = String> {
        prop_oneof![
            "[A-Z0-9]{16}".prop_map(|s| format!("AKIA{s}")),
            "[A-Za-z0-9]{36}".prop_map(|s| format!("ghp_{s}")),
            "[A-Za-z0-9]{24,48}".prop_map(|s| format!("sk-{s}")),
            "[A-Za-z0-9]{10,30}".prop_map(|s| format!("xoxp-0000-{s}")),
            (
                "[A-Za-z0-9_-]{8,20}",
                "[A-Za-z0-9_-]{8,20}",
                "[A-Za-z0-9_-]{8,40}"
            )
                .prop_map(|(a, b, c)| format!("eyJ{a}.eyJ{b}.{c}")),
            "[0-9]{15}".prop_map(|s| card(&format!("4{s}"))),
            "[A-Za-z0-9+/]{20,200}".prop_map(|s| format!(
                "-----BEGIN RSA PRIVATE KEY-----\n{s}\n-----END RSA PRIVATE KEY-----"
            )),
            "[A-Za-z0-9]{8,20}".prop_map(|s| format!("password = {s}")),
        ]
    }

    /// Turns 16 digits into a card number by fixing the last one so the Luhn check passes.
    fn card(digits: &str) -> String {
        let mut d: Vec<u32> = digits.chars().filter_map(|c| c.to_digit(10)).collect();
        d.pop();
        let sum: u32 = d
            .iter()
            .rev()
            .enumerate()
            .map(|(i, &x)| {
                if i % 2 == 0 {
                    let y = x * 2;
                    if y > 9 { y - 9 } else { y }
                } else {
                    x
                }
            })
            .sum();
        d.push((10 - sum % 10) % 10);
        d.iter()
            .map(|x| char::from_digit(*x, 10).unwrap_or('0'))
            .collect()
    }

    /// The property test's names, set for each case, read by one scrubber built once.
    static CASE_NAMES: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

    struct CaseNames;

    impl crate::scrub::Detector for CaseNames {
        fn find(&self, text: &str) -> Result<Vec<crate::scrub::Found>, crate::scrub::EntityError> {
            let names = CASE_NAMES.lock().map(|n| n.clone()).unwrap_or_default();
            Names(names).find(text)
        }
    }

    static CASE_SCRUBBER: LazyLock<crate::scrub::Scrubber> =
        LazyLock::new(|| crate::scrub::Scrubber::with_detector(Box::new(CaseNames)).unwrap());
    static CASE_KEYED: LazyLock<crate::scrub::surrogate::Keyed> =
        LazyLock::new(|| crate::scrub::surrogate::Keyed::from_secret(&"3c".repeat(32)).unwrap());

    /// A fake name: one or two capitalised made-up words.
    fn fake_name() -> impl Strategy<Value = String> {
        ("[A-Z][a-z]{3,8}", proptest::option::of("[A-Z][a-z]{3,9}")).prop_map(|(first, last)| {
            match last {
                Some(last) => format!("{first} {last}"),
                None => first,
            }
        })
    }

    /// A fake structured detail the patterns find.
    fn fake_detail() -> impl Strategy<Value = String> {
        prop_oneof![
            "[a-z]{3,8}".prop_map(|s| format!("{s}.fake@example.com")),
            "[0-9]{6}".prop_map(|s| format!("07700 {s}")),
            (1u8..=28, 0usize..12, 1950u16..2030).prop_map(|(d, m, y)| {
                const M: [&str; 12] = [
                    "January",
                    "February",
                    "March",
                    "April",
                    "May",
                    "June",
                    "July",
                    "August",
                    "September",
                    "October",
                    "November",
                    "December",
                ];
                format!("{d} {} {y}", M[m])
            }),
            "[0-9]{8}".prop_map(|s| format!("account {s}")),
        ]
    }

    proptest! {
        /// No generated fake personal detail reaches what would be sent, in any message, and the
        /// same conversation scrubs the same way twice.
        #[test]
        fn no_fake_personal_detail_leaves(
            name in fake_name(),
            detail in fake_detail(),
            filler in "[a-z ,.?]{0,40}",
        ) {
            *CASE_NAMES.lock().unwrap() = vec![name.clone()];
            let scrub = |body: &mut Value| {
                crate::scrub::scrub_conversation(&CASE_SCRUBBER, &CASE_KEYED, body)
            };
            let body = json!({ "model": "m", "messages": [
                { "role": "user", "content": format!("{filler} {name} {detail} {filler}") },
                { "role": "assistant", "content": format!("Hello {name}.") },
                { "role": "user", "content": format!("and {} again", name.to_lowercase()) },
            ]});
            let prepared = FLOOR.prepare_with(body.clone(), &SCRUB_ROUTE, Some(&scrub));
            prop_assert!(prepared.is_ok(), "blocked: {:?}", prepared.err());
            let sent = prepared.map(|p| p.body.to_string().to_lowercase()).unwrap_or_default();
            for word in name.split(' ') {
                let word = word.to_lowercase();
                let whole = sent.split(|c: char| !c.is_alphanumeric()).any(|w| w == word);
                prop_assert!(!whole, "{} left in: {}", word, sent);
            }
            let detail_core = detail.trim_start_matches("account ").to_lowercase();
            prop_assert!(!sent.contains(&detail_core), "{} left in: {}", detail, sent);
            let again = FLOOR.prepare_with(body, &SCRUB_ROUTE, Some(&scrub)).map(|p| p.body.to_string().to_lowercase());
            prop_assert_eq!(again.unwrap_or_default(), sent);
        }

        /// No generated fake credential ever reaches what would be sent: it is found, a route that
        /// doesn't strip is refused, and stripping removes it in a way the outbound check accepts.
        #[test]
        fn no_fake_credential_leaves(
            before in "[a-z ,.?\n]{0,60}",
            fake in fake_credential(),
            after in "[a-z ,.?\n]{0,60}",
        ) {
            let (keeps, strips) = &*ROUTES;
            let body = ask(&format!("{before} {fake} {after}"));
            prop_assert!(FLOOR.credentials_found(&body), "not found: {}", fake);
            prop_assert_eq!(
                FLOOR.prepare(body.clone(), keeps).unwrap_err(),
                FloorError::CredentialsLeft
            );
            // Stripped cleanly: the outbound check agrees, and the value is gone.
            let prepared = FLOOR.prepare(body, strips);
            prop_assert!(prepared.is_ok(), "blocked: {} ({:?})", fake, prepared.err());
            let secret = fake.rsplit(' ').next().unwrap_or(&fake);
            let sent = prepared.map(|p| p.body.to_string()).unwrap_or_default();
            prop_assert!(!sent.contains(secret), "left in: {}", fake);
        }
    }
}
