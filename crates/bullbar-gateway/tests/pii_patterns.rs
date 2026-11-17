//! The scrubber's pattern part against the synthetic PII set (`scenarios/pii/messages.toml`): every
//! structured detail (emails, phone numbers, identifiers, dates, addresses) is found whole, and the
//! clean messages and public figures stay untouched. Names, places and organisations are the entity
//! part's job (slice 2), relationships the context pass's (slice 6).

use std::path::PathBuf;

use bullbar_gateway::scrub::surrogate::Keyed;
use bullbar_gateway::scrub::{PiiKind, Scrubber, scrub_conversation};
use serde::Deserialize;

#[derive(Deserialize)]
struct Set {
    case: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    text: String,
    spans: Vec<Span>,
}

#[derive(Deserialize)]
struct Span {
    kind: String,
    text: String,
    nth: Option<usize>,
}

fn set() -> Set {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scenarios/pii/messages.toml");
    toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// Where the labelled span sits in the message (its `nth` occurrence).
fn locate(text: &str, span: &Span) -> std::ops::Range<usize> {
    let mut from = 0;
    let mut at = None;
    for _ in 0..span.nth.unwrap_or(1) {
        let found = text[from..].find(&span.text).map(|i| i + from).unwrap();
        at = Some(found);
        from = found + 1;
    }
    let start = at.unwrap();
    start..start + span.text.len()
}

const STRUCTURED: &[&str] = &["email", "phone", "id", "date", "address"];

#[test]
fn every_structured_detail_is_found_whole() {
    let scrubber = Scrubber::patterns_only().unwrap();
    let mut missed = Vec::new();
    for case in set().case {
        let found = scrubber.find(&case.text).unwrap();
        for span in case
            .spans
            .iter()
            .filter(|s| STRUCTURED.contains(&s.kind.as_str()))
        {
            let want = locate(&case.text, span);
            let whole = found
                .iter()
                .any(|f| f.range.start <= want.start && f.range.end >= want.end);
            if !whole {
                missed.push(format!("{} {:?} in {:?}", span.kind, span.text, case.text));
            }
        }
    }
    assert!(missed.is_empty(), "missed:\n{}", missed.join("\n"));
}

#[test]
fn clean_messages_and_public_figures_are_left_alone() {
    let scrubber = Scrubber::patterns_only().unwrap();
    let mut alarms = Vec::new();
    for case in set().case {
        let only_public = case.spans.iter().all(|s| s.kind == "public");
        if !only_public {
            continue;
        }
        for f in scrubber.find(&case.text).unwrap() {
            alarms.push(format!(
                "{:?} {:?} in {:?}",
                f.kind, &case.text[f.range], case.text
            ));
        }
    }
    assert!(
        alarms.is_empty(),
        "found in clean text:\n{}",
        alarms.join("\n")
    );
}

#[test]
fn structured_details_leave_as_tokens_the_same_value_the_same_token() {
    let scrubber = Scrubber::patterns_only().unwrap();
    let keyed = Keyed::from_secret(&"7e".repeat(32)).unwrap();
    let mut body = serde_json::json!({ "messages": [{ "role": "user", "content":
        "Call me on 07700 900123 or 07700 900123, or write to imogen.fakeworth@example.com." }] });
    let replaced = scrub_conversation(&scrubber, &keyed, &mut body).unwrap();
    assert_eq!(
        body["messages"][0]["content"],
        "Call me on [PHONE_1] or [PHONE_1], or write to [EMAIL_1]."
    );
    let entries: Vec<_> = replaced.entries().collect();
    assert_eq!(
        entries,
        [
            (PiiKind::Phone, "07700 900123", "[PHONE_1]"),
            (PiiKind::Email, "imogen.fakeworth@example.com", "[EMAIL_1]"),
        ]
    );
}
