//! The whole scrubber (patterns and the entity model) against the synthetic PII set. Needs the model,
//! which isn't in the repository: set `BULLBAR_PII_MODEL` to its folder and run
//! `cargo nextest run --run-ignored only pii_entities`. The floors are what was measured when the
//! model was adopted (2026-11-16); a change mustn't drop below them.

use std::collections::BTreeMap;
use std::path::PathBuf;

use bullbar_gateway::scrub::Scrubber;
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

fn locate(text: &str, span: &Span) -> std::ops::Range<usize> {
    let mut from = 0;
    let mut start = 0;
    for _ in 0..span.nth.unwrap_or(1) {
        start = text[from..].find(&span.text).map(|i| i + from).unwrap();
        from = start + 1;
    }
    start..start + span.text.len()
}

/// Found, out of, per kind; and clean messages with any find.
fn measure() -> (BTreeMap<String, (usize, usize)>, usize, usize, usize) {
    let model =
        std::env::var_os("BULLBAR_PII_MODEL").expect("BULLBAR_PII_MODEL names the model folder");
    let scrubber = Scrubber::with_entities(&PathBuf::from(model)).unwrap();
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scenarios/pii/messages.toml");
    let set: Set = toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let mut per_kind: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let (mut clean, mut alarmed, mut extras) = (0, 0, 0);
    for case in &set.case {
        let found = scrubber.find(&case.text).unwrap();
        if case.spans.iter().all(|s| s.kind == "public") {
            clean += 1;
            if !found.is_empty() {
                alarmed += 1;
                eprintln!(
                    "alarm: {:?}",
                    found
                        .iter()
                        .map(|f| &case.text[f.range.clone()])
                        .collect::<Vec<_>>()
                );
            }
        }
        if !case.spans.iter().all(|s| s.kind == "public") {
            for f in &found {
                let labelled = case.spans.iter().any(|s| {
                    let want = locate(&case.text, s);
                    f.range.start < want.end && want.start < f.range.end
                });
                if !labelled {
                    extras += 1;
                    eprintln!("extra {:?}", &case.text[f.range.clone()]);
                }
            }
        }
        for span in case.spans.iter().filter(|s| s.kind != "public") {
            let want = locate(&case.text, span);
            let hit = found
                .iter()
                .any(|f| f.range.start < want.end && want.start < f.range.end);
            let entry = per_kind.entry(span.kind.clone()).or_default();
            entry.1 += 1;
            if hit {
                entry.0 += 1;
            } else {
                eprintln!("missed {} {:?}", span.kind, span.text);
            }
        }
    }
    (per_kind, clean, alarmed, extras)
}

#[test]
#[ignore = "needs the entity model: set BULLBAR_PII_MODEL"]
fn the_scrubber_finds_at_least_what_was_measured() {
    let (per_kind, clean, alarmed, extras) = measure();
    let (found, total) = per_kind
        .values()
        .fold((0, 0), |(f, t), (a, b)| (f + a, t + b));
    eprintln!(
        "{per_kind:?}\nfound {found}/{total}, clean messages with a find {alarmed}/{clean}, extras {extras}"
    );
    for (kind, (hit, of)) in &per_kind {
        if kind == "relationship" {
            // The context pass's job (slice 6); the model catches some as people.
            assert!(*hit >= 11, "relationships found {hit}/{of}, measured 11");
        } else {
            assert_eq!(hit, of, "{kind}: every one was found when measured");
        }
    }
    // 102 when adopted; 108 with the title-request case (2026-11-16, all six found).
    assert!(found >= 108, "found {found}/{total}, measured 108");
    // Ceilings on false finds, so a filter change can't quietly let the noise back in.
    assert!(
        alarmed <= 11,
        "clean messages with a find {alarmed}, measured 11"
    );
    // 9 when adopted; 7 once lowercase phrases count as people only as relationships (2026-11-17).
    assert!(extras <= 7, "extra finds {extras}, measured 7");
}

/// A long text is read to its end. The model library stops after 512 of its tokens and says nothing,
/// so a detail past that point would leave unscrubbed and unchecked (found in the chunk 3 hand check,
/// a chat client's title request carrying the whole conversation in one message).
#[test]
#[ignore = "needs the entity model: set BULLBAR_PII_MODEL"]
fn a_long_text_is_read_to_its_end() {
    let model =
        std::env::var_os("BULLBAR_PII_MODEL").expect("BULLBAR_PII_MODEL names the model folder");
    let scrubber = Scrubber::with_entities(&PathBuf::from(model)).unwrap();
    let filler = "The weather was mild, and the garden needed work. ".repeat(60);
    let text = format!(
        "Imogen Fakeworth wrote first. {filler}Then Kwabena Fakeson visited Tobermory. {filler}\
         Finally Priya Fakesmith phoned."
    );
    let found: Vec<&str> = scrubber
        .find(&text)
        .unwrap()
        .iter()
        .map(|f| &text[f.range.clone()])
        .collect();
    for name in [
        "Imogen Fakeworth",
        "Kwabena Fakeson",
        "Tobermory",
        "Priya Fakesmith",
    ] {
        assert!(
            found.iter().any(|f| f.contains(name)),
            "{name} missed: {found:?}"
        );
    }
}

#[test]
#[ignore = "needs the entity model: set BULLBAR_PII_MODEL"]
fn an_empty_message_finds_nothing() {
    // A chat client sent an empty message (2026-11-17); the model failed on it and the request was
    // blocked. Empty and blank texts hold nothing to find.
    let model =
        std::env::var_os("BULLBAR_PII_MODEL").expect("BULLBAR_PII_MODEL names the model folder");
    let scrubber = Scrubber::with_entities(&PathBuf::from(model)).unwrap();
    for text in ["", " ", "\n\t "] {
        assert!(scrubber.find(text).unwrap().is_empty(), "{text:?}");
    }
}
