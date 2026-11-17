//! Scrubbing a whole conversation (ADR 0020, points 3 to 5). Clients resend the conversation each
//! turn, so every request is scrubbed whole: details are found in every message, each distinct value
//! gets one surrogate, and that value is replaced wherever it appears, in every message (found once,
//! replaced everywhere). The map lives only as long as the request; the same conversation gets the
//! same surrogates on the next turn because they are made from the value, the conversation's first
//! user message and the per-install secret, not stored.

use std::fmt;

use regex::{Regex, RegexBuilder};
use serde_json::Value;

use super::entities::is_relationship;
use super::surrogate::{Keyed, candidate};
use super::{PiiKind, ScrubError, Scrubber, spaced_pattern, squashed};

/// One detail replaced: what it was, what it became.
#[derive(Clone, PartialEq, Eq)]
struct Entry {
    kind: PiiKind,
    original: String,
    surrogate: String,
    /// In the latest user message: expected back in the answer, so named if it isn't.
    latest: bool,
}

/// What was replaced in a request, for the outbound check and for restoring the reply. Its debug
/// output hides the values.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Replaced(Vec<Entry>);

impl Replaced {
    /// How many distinct details were replaced.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// True when nothing was replaced.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The original values, for the outbound check.
    pub fn originals(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(|e| e.original.as_str())
    }

    /// Each detail: its kind, the original and the surrogate, for restoring and for `bullbar scrub`.
    pub fn entries(&self) -> impl Iterator<Item = (PiiKind, &str, &str)> {
        self.0
            .iter()
            .map(|e| (e.kind, e.original.as_str(), e.surrogate.as_str()))
    }

    /// Whether this original is in the latest user message (details from earlier turns or a
    /// system prompt aren't expected back in this answer).
    pub(crate) fn in_latest(&self, original: &str) -> bool {
        self.0.iter().any(|e| e.original == original && e.latest)
    }
}

#[cfg(test)]
impl Replaced {
    /// A map written out, for tests: (kind, original, surrogate).
    pub(crate) fn from_entries(entries: &[(PiiKind, &str, &str)]) -> Self {
        Replaced(
            entries
                .iter()
                .map(|(kind, original, surrogate)| Entry {
                    kind: *kind,
                    original: (*original).to_owned(),
                    surrogate: (*surrogate).to_owned(),
                    latest: true,
                })
                .collect(),
        )
    }
}

impl fmt::Debug for Replaced {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Replaced({} hidden)", self.0.len())
    }
}

/// How many candidates to try before a value gets a token instead.
const ATTEMPTS: u32 = 32;

/// Scrubs every message's text in a chat body in place, and says what was replaced. An error means
/// the conversation wasn't fully checked, and nothing may leave.
pub fn scrub_conversation(
    scrubber: &Scrubber,
    keyed: &Keyed,
    body: &mut Value,
) -> Result<Replaced, ScrubError> {
    let mut texts: Vec<String> = Vec::new();
    each_text(body, &mut |t| texts.push(t.clone()));
    let seed = keyed.hash(&[b"conversation", user_text(body, First).as_bytes()]);
    let latest_text = squashed(&user_text(body, Last));

    // Every distinct value, in the order it first appears (so the map is the same on every turn).
    let placeholder = Regex::new(r"\[[A-Z]+_\d+\]")?;
    let mut found: Vec<(PiiKind, String)> = Vec::new();
    for text in &texts {
        for f in scrubber.find(text)? {
            let value = text[f.range].trim().to_owned();
            let worth = value.chars().filter(|c| c.is_alphanumeric()).count() >= 2;
            if worth && !placeholder.is_match(&value) && !found.iter().any(|(_, v)| same(v, &value))
            {
                let kind = relationship_or(f.kind, &value);
                let parts = name_parts(kind, &value);
                found.push((kind, value));
                // A full name's first and last words are found with it, so "Imogen" alone doesn't
                // leak where only "Imogen Fakeworth" was found.
                for part in parts {
                    if !found.iter().any(|(_, v)| same(v, &part)) {
                        found.push((PiiKind::Person, part));
                    }
                }
            }
        }
    }
    if found.is_empty() {
        return Ok(Replaced::default());
    }

    let all_text = texts.join("\n").to_lowercase();
    let mut taken: Vec<Entry> = Vec::new();
    for (kind, original) in found {
        let surrogate = assign(keyed, seed, kind, &original, &taken, &all_text);
        let latest = contains_word(&latest_text, &squashed(&original));
        taken.push(Entry {
            kind,
            original,
            surrogate,
            latest,
        });
    }

    let finder = finder(&taken)?;
    each_text_mut(body, &mut |t| *t = replace(t, &finder, &taken));
    Ok(Replaced(taken))
}

/// One message scrubbed as a conversation of its own: what it would leave as, and what was
/// replaced. For `bullbar scrub`.
pub fn scrub_text(
    scrubber: &Scrubber,
    keyed: &Keyed,
    text: &str,
) -> Result<(String, Replaced), ScrubError> {
    let mut body = serde_json::json!({ "messages": [{ "role": "user", "content": text }] });
    let replaced = scrub_conversation(scrubber, keyed, &mut body)?;
    let leaves = body["messages"][0]["content"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    Ok((leaves, replaced))
}

/// The surrogate for one value: a realistic one if its kind has them and one is free, else a token.
fn assign(
    keyed: &Keyed,
    seed: u64,
    kind: PiiKind,
    original: &str,
    taken: &[Entry],
    all_text: &str,
) -> String {
    if kind == PiiKind::Person
        && let Some(part) = part_of_known_name(original, taken)
    {
        return part;
    }
    let known_first = (kind == PiiKind::Person)
        .then(|| known_first_name(original, taken))
        .flatten();
    let original_words: Vec<String> = taken
        .iter()
        .map(|e| e.original.as_str())
        .chain([original])
        .flat_map(|o| {
            o.split_whitespace()
                .map(str::to_lowercase)
                .collect::<Vec<_>>()
        })
        .collect();
    // Names and places mustn't share a word with any original ("Imogen" for "Imogen Fakeworth");
    // dates share words like "of" and months by nature, and differ as a whole.
    let shares_words = matches!(kind, PiiKind::Person | PiiKind::Place);
    let free = |c: &str| {
        let lower = c.to_lowercase();
        !taken.iter().any(|e| e.surrogate.to_lowercase() == lower)
            && !contains_word(all_text, c)
            && !(shares_words
                && c.split_whitespace()
                    .any(|w| original_words.contains(&w.to_lowercase())))
    };
    for attempt in 0..ATTEMPTS {
        let c = candidate(keyed, seed, kind, original, attempt).map(|c| match &known_first {
            // Keep the first name already given to this person.
            Some(first) => {
                let rest: Vec<&str> = c.split_whitespace().skip(1).collect();
                format!("{first} {}", rest.join(" "))
            }
            None => c,
        });
        match c {
            Some(c) if free(&c) => return c,
            Some(_) => {}
            None => break,
        }
    }
    // A token: the next free number for this kind.
    let mut n = taken
        .iter()
        .filter(|e| e.surrogate.starts_with(&format!("[{}_", kind.label())))
        .count()
        + 1;
    loop {
        let token = format!("[{}_{n}]", kind.label());
        // Free in the text and not given to another detail in this request.
        if !all_text.contains(&token.to_lowercase()) && !taken.iter().any(|e| e.surrogate == token)
        {
            return token;
        }
        n += 1;
    }
}

/// The first and last words of a person's name of two words or more (titles and initials left out).
fn name_parts(kind: PiiKind, value: &str) -> Vec<String> {
    let words: Vec<&str> = value.split_whitespace().collect();
    if kind != PiiKind::Person || words.len() < 2 {
        return Vec::new();
    }
    [words[0], words[words.len() - 1]]
        .into_iter()
        .filter(|w| w.chars().filter(|c| c.is_alphabetic()).count() >= 2)
        .map(str::to_owned)
        .collect()
}

/// A "person" the model found that is a relationship ("my wife", "her teacher"): a fake name there
/// would change what the message says, so it gets a token until the context pass can generalise it
/// (slice 6).
fn relationship_or(kind: PiiKind, value: &str) -> PiiKind {
    if kind == PiiKind::Person && is_relationship(&value.to_lowercase()) {
        PiiKind::Relationship
    } else {
        kind
    }
}

/// The same detail, ignoring case and which space characters separate its words.
fn same(a: &str, b: &str) -> bool {
    squashed(a) == squashed(b)
}

/// Names already replaced, as (original words, surrogate words), where the two have the same shape.
fn known_names(taken: &[Entry]) -> impl Iterator<Item = (Vec<&str>, Vec<&str>)> {
    taken
        .iter()
        .filter(|e| e.kind == PiiKind::Person && !e.surrogate.starts_with('['))
        .map(|e| {
            (
                e.original.split_whitespace().collect::<Vec<_>>(),
                e.surrogate.split_whitespace().collect::<Vec<_>>(),
            )
        })
        .filter(|(known, given)| known.len() == given.len())
}

/// A one-word name that is the first or last word of a name already replaced ("Imogen" after
/// "Imogen Fakeworth") takes the matching part, so one person stays one person.
fn part_of_known_name(original: &str, taken: &[Entry]) -> Option<String> {
    let [one] = original.split_whitespace().collect::<Vec<_>>()[..] else {
        return None;
    };
    known_names(taken)
        .filter(|(known, _)| known.len() > 1)
        .find_map(|(known, given)| {
            if same(one, known[0]) {
                Some(given[0].to_owned())
            } else if same(one, known[known.len() - 1]) {
                Some(given[given.len() - 1].to_owned())
            } else {
                None
            }
        })
}

/// For a longer name whose first word was already replaced alone ("Imogen Fakeworth" after
/// "Imogen"): that first name's surrogate.
fn known_first_name(original: &str, taken: &[Entry]) -> Option<String> {
    let words: Vec<&str> = original.split_whitespace().collect();
    if words.len() < 2 {
        return None;
    }
    known_names(taken)
        .find(|(known, _)| known.len() == 1 && same(known[0], words[0]))
        .map(|(_, given)| given[0].to_owned())
}

/// Whether `word` appears in `lower_text` (already lowercased) as a whole word, ignoring case.
fn contains_word(lower_text: &str, word: &str) -> bool {
    let word = word.to_lowercase();
    let mut from = 0;
    while let Some(i) = lower_text[from..].find(&word) {
        let start = from + i;
        if bounded(lower_text, start, start + word.len()) {
            return true;
        }
        from = start + lower_text[start..].chars().next().map_or(1, char::len_utf8);
    }
    false
}

/// True when the match isn't part of a longer word: no letter or digit right before or after.
fn bounded(text: &str, start: usize, end: usize) -> bool {
    let before = text[..start].chars().next_back();
    let after = text[end..].chars().next();
    !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
}

/// One case-insensitive search for every original, longest first, so "Imogen Fakeworth" wins over
/// "Imogen" where both match.
fn finder(taken: &[Entry]) -> Result<Regex, regex::Error> {
    let mut originals: Vec<&str> = taken.iter().map(|e| e.original.as_str()).collect();
    originals.sort_by_key(|o| std::cmp::Reverse(o.len()));
    let alternation: Vec<String> = originals.iter().map(|o| spaced_pattern(o)).collect();
    RegexBuilder::new(&alternation.join("|"))
        .case_insensitive(true)
        .build()
}

/// `text` with every whole-word occurrence of an original replaced by its surrogate. An occurrence
/// in lowercase gets the surrogate in lowercase (tokens stay as they are).
fn replace(text: &str, finder: &Regex, taken: &[Entry]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut at = 0;
    while let Some(m) = finder.find_at(text, at) {
        let entry = taken.iter().find(|e| same(&e.original, m.as_str()));
        match entry {
            Some(e) if bounded(text, m.start(), m.end()) => {
                out.push_str(&text[copied..m.start()]);
                let lower = m
                    .as_str()
                    .chars()
                    .filter(|c| c.is_alphabetic())
                    .all(char::is_lowercase);
                if lower && !e.surrogate.starts_with('[') {
                    out.push_str(&e.surrogate.to_lowercase());
                } else {
                    out.push_str(&e.surrogate);
                }
                copied = m.end();
                at = m.end();
            }
            _ => at = m.start() + text[m.start()..].chars().next().map_or(1, char::len_utf8),
        }
    }
    out.push_str(&text[copied..]);
    out
}

/// Which user message.
#[derive(Clone, Copy)]
enum Which {
    First,
    Last,
}
use Which::{First, Last};

/// The text of the first user message (the conversation's seed) or the last (what this answer
/// answers); empty if there is none.
fn user_text(body: &Value, which: Which) -> String {
    let Some(messages) = body.get("messages").and_then(Value::as_array) else {
        return String::new();
    };
    let is_user = |m: &&Value| m.get("role").and_then(Value::as_str) == Some("user");
    let found = match which {
        First => messages.iter().find(is_user),
        Last => messages.iter().rev().find(is_user),
    };
    let Some(first) = found else {
        return String::new();
    };
    let mut text = String::new();
    each_content(first.get("content"), &mut |t| text.push_str(t));
    text
}

fn each_content(content: Option<&Value>, f: &mut impl FnMut(&String)) {
    match content {
        Some(Value::String(s)) => f(s),
        Some(Value::Array(parts)) => {
            for part in parts {
                if let Some(Value::String(s)) = part.get("text") {
                    f(s);
                }
            }
        }
        _ => {}
    }
}

/// Calls `f` on every message's text: `content` as a string, or the `text` of each content part.
fn each_text(body: &Value, f: &mut impl FnMut(&String)) {
    if let Some(messages) = body.get("messages").and_then(Value::as_array) {
        for m in messages {
            each_content(m.get("content"), f);
        }
    }
}

fn each_text_mut(body: &mut Value, f: &mut impl FnMut(&mut String)) {
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    for m in messages {
        match m.get_mut("content") {
            Some(Value::String(s)) => f(s),
            Some(Value::Array(parts)) => {
                for part in parts {
                    if let Some(Value::String(s)) = part.get_mut("text") {
                        f(s);
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scrub::{Detector, EntityError, Found};
    use serde_json::json;

    /// Finds the listed names as people.
    struct Names(&'static [&'static str]);

    impl Detector for Names {
        fn find(&self, text: &str) -> Result<Vec<Found>, EntityError> {
            Ok(self
                .0
                .iter()
                .flat_map(|n| text.match_indices(n).map(|(at, n)| at..at + n.len()))
                .map(|range| Found {
                    range,
                    kind: PiiKind::Person,
                })
                .collect())
        }
    }

    fn scrubber(names: &'static [&'static str]) -> Scrubber {
        Scrubber::with_detector(Box::new(Names(names))).unwrap()
    }

    fn keyed() -> Keyed {
        Keyed::from_secret(&"5a".repeat(32)).unwrap()
    }

    fn chat(messages: &[(&str, &str)]) -> Value {
        let messages: Vec<Value> = messages
            .iter()
            .map(|(role, content)| json!({ "role": role, "content": content }))
            .collect();
        json!({ "model": "m", "messages": messages })
    }

    fn texts(body: &Value) -> Vec<String> {
        let mut out = Vec::new();
        each_text(body, &mut |t| out.push(t.clone()));
        out
    }

    #[test]
    fn found_once_is_replaced_everywhere_including_earlier_answers() {
        // The assistant's earlier answer was restored, so it holds the real name; the detector
        // finds the name only in the user's message, and it goes from both.
        let mut body = chat(&[
            (
                "user",
                "Write a card for Imogen Fakeworth, call 07700 900123.",
            ),
            (
                "assistant",
                "Dear Imogen Fakeworth, ... (and imogen fakeworth again)",
            ),
            ("user", "Shorter please."),
        ]);
        let replaced =
            scrub_conversation(&scrubber(&["Imogen Fakeworth"]), &keyed(), &mut body).unwrap();
        let sent = body.to_string().to_lowercase();
        assert!(
            !sent.contains("imogen") && !sent.contains("fakeworth"),
            "{sent}"
        );
        assert!(!sent.contains("07700"), "{sent}");
        // The name, its first and last words, the phone number.
        assert_eq!(replaced.len(), 4);
        let (_, _, name) = replaced.entries().next().unwrap();
        assert!(body.to_string().contains(name), "the surrogate is used");
    }

    /// An earlier answer can hold the name with another space character between its words (a
    /// provider's narrow no-break space, kept by the client): still one person, one surrogate (found
    /// in the chunk 3 hand check, where it became a second surname).
    #[test]
    fn a_name_with_another_space_between_its_words_is_the_same_name() {
        let mut body = chat(&[
            ("user", "A card for Imogen Fakeworth please."),
            ("assistant", "Happy birthday, Imogen\u{202f}Fakeworth!"),
            ("user", "Shorter."),
        ]);
        let replaced = scrub_conversation(
            &scrubber(&["Imogen Fakeworth", "Imogen\u{202f}Fakeworth"]),
            &keyed(),
            &mut body,
        )
        .unwrap();
        // The name, its first and last words: no second entry for the other spelling.
        assert_eq!(replaced.len(), 3, "{replaced:?}");
        let (_, _, name) = replaced.entries().next().unwrap();
        let t = texts(&body);
        assert!(t[0].contains(name) && t[1].contains(name), "{t:?}");
    }

    /// "Not in the answer" is about the latest message: details from earlier turns, or a client's
    /// system prompt, have no reason to come back in this answer (noise seen in the chunk 3 hand
    /// check: an earlier turn's phone listed as missing).
    #[test]
    fn only_the_latest_messages_details_are_expected_back() {
        let mut body = chat(&[
            ("system", "Today is the 3rd of June."),
            ("user", "A card for Imogen Fakeworth, call 07700 900123."),
            ("assistant", "Done."),
            ("user", "Make it warmer for Imogen."),
        ]);
        let replaced =
            scrub_conversation(&scrubber(&["Imogen Fakeworth"]), &keyed(), &mut body).unwrap();
        let first = replaced
            .entries()
            .find(|(_, o, _)| *o == "Imogen")
            .map(|(_, _, s)| s.to_owned())
            .unwrap();
        let restorer = crate::scrub::Restorer::new(&replaced).unwrap();
        let report = restorer.report(&format!("Dear {first}, warm wishes."));
        assert!(report.is_empty(), "{report:?}");
        // A detail from the latest message that didn't come back is still named.
        assert!(!restorer.report("Warm wishes.").is_empty());
    }

    /// Two details never share a token. Found in the chunk 3 hand check: `[ADDRESS_1]` was already
    /// in the text (a provider's reasoning quoted it), so the street became `[ADDRESS_2]`, and so did
    /// the whole address after it: the count of tokens given skipped the one the text held.
    #[test]
    fn a_token_already_given_is_never_given_again() {
        let entry = |original: &str, surrogate: &str| Entry {
            kind: PiiKind::Address,
            original: original.to_owned(),
            surrogate: surrogate.to_owned(),
            latest: true,
        };
        let taken = [entry("12 Fakery Lane", "[ADDRESS_2]")];
        let all_text = "they gave placeholder [address_1]; 12 fakery lane, tobermory";
        let token = assign(
            &keyed(),
            0,
            PiiKind::Address,
            "12 Fakery Lane, Tobermory",
            &taken,
            all_text,
        );
        assert_eq!(token, "[ADDRESS_3]");
    }

    #[test]
    fn the_same_conversation_gets_the_same_surrogates_on_every_turn() {
        let first = [(
            "user",
            "Ask Imogen about the 14 March 1987 photo in Tobermory.",
        )];
        let names = &["Imogen"];
        let mut one = chat(&first);
        let mut two = chat(&[first[0], ("assistant", "Sure."), ("user", "And Kwame too.")]);
        let a = scrub_conversation(&scrubber(names), &keyed(), &mut one).unwrap();
        let b = scrub_conversation(&scrubber(&["Imogen", "Kwame"]), &keyed(), &mut two).unwrap();
        assert_eq!(texts(&one)[0], texts(&two)[0]);
        let a: Vec<_> = a.entries().collect();
        let b: Vec<_> = b.entries().collect();
        assert_eq!(a[..], b[..a.len()]);
    }

    #[test]
    fn different_conversations_get_different_surrogates() {
        let names = &["Imogen Fakeworth", "Kwame Testa", "Priya Vanterpool"];
        let surrogates = |opening: &str| {
            let mut body = chat(&[(
                "user",
                &format!("{opening} Imogen Fakeworth, Kwame Testa and Priya Vanterpool"),
            )]);
            scrub_conversation(&scrubber(names), &keyed(), &mut body).unwrap();
            texts(&body)[0].clone()
        };
        assert_ne!(surrogates("Hello:"), surrogates("Hi:"));
    }

    #[test]
    fn a_full_name_found_once_takes_its_parts_with_it() {
        // The detector finds only the full name; the first name alone must not leak.
        let mut body = chat(&[(
            "user",
            "I am Imogen Fakeworth. Imogen is a terrible baker; ask the Fakeworth family.",
        )]);
        let replaced =
            scrub_conversation(&scrubber(&["Imogen Fakeworth"]), &keyed(), &mut body).unwrap();
        let sent = texts(&body)[0].to_lowercase();
        assert!(
            !sent.contains("imogen") && !sent.contains("fakeworth"),
            "{sent}"
        );
        let entries: Vec<_> = replaced.entries().collect();
        let full: Vec<&str> = entries[0].2.split(' ').collect();
        assert_eq!(entries[1].2, full[0]);
        assert_eq!(entries[2].2, full[1]);
    }

    #[test]
    fn a_date_leaves_shifted_not_as_a_token() {
        let mut body = chat(&[(
            "user",
            "The party is on 22nd of August, and the 2026-12-03 deadline.",
        )]);
        let replaced =
            scrub_conversation(&Scrubber::patterns_only().unwrap(), &keyed(), &mut body).unwrap();
        for (kind, _, surrogate) in replaced.entries() {
            assert_eq!(kind, PiiKind::Date);
            assert!(!surrogate.starts_with('['), "{surrogate}");
        }
        assert_eq!(replaced.len(), 2);
    }

    #[test]
    fn one_person_stays_one_person() {
        let mut body = chat(&[(
            "user",
            "Imogen Fakeworth called. Imogen said Fakeworth is her name.",
        )]);
        let replaced = scrub_conversation(
            &scrubber(&["Imogen Fakeworth", "Imogen", "Fakeworth"]),
            &keyed(),
            &mut body,
        )
        .unwrap();
        let entries: Vec<_> = replaced.entries().collect();
        let full: Vec<&str> = entries[0].2.split(' ').collect();
        assert_eq!(entries[1].2, full[0]);
        assert_eq!(entries[2].2, full[1]);
    }

    #[test]
    fn a_relationship_becomes_a_token_and_placeholders_are_left_alone() {
        let mut body = chat(&[("user", "my wife has the key [CREDENTIAL_1]")]);
        let replaced = scrub_conversation(
            &scrubber(&["my wife", "[CREDENTIAL_1]"]),
            &keyed(),
            &mut body,
        )
        .unwrap();
        assert_eq!(
            texts(&body)[0],
            "[RELATIONSHIP_1] has the key [CREDENTIAL_1]"
        );
        assert_eq!(replaced.len(), 1);
    }

    #[test]
    fn a_surrogate_never_collides_with_words_already_there() {
        // Whatever the picks, no surrogate equals a word in the conversation.
        for seed_word in ["Alex", "Ben", "Jane", "Emma", "Leo"] {
            let mut body = chat(&[("user", &format!("{seed_word} and Imogen and Kwame"))]);
            let replaced =
                scrub_conversation(&scrubber(&["Imogen", "Kwame"]), &keyed(), &mut body).unwrap();
            for (_, _, s) in replaced.entries() {
                assert_ne!(s.to_lowercase(), seed_word.to_lowercase());
            }
        }
    }

    #[test]
    fn a_failing_detector_is_an_error() {
        struct Broken;
        impl Detector for Broken {
            fn find(&self, _: &str) -> Result<Vec<Found>, EntityError> {
                Err(EntityError::Inference("broken".into()))
            }
        }
        let scrubber = Scrubber::with_detector(Box::new(Broken)).unwrap();
        let mut body = chat(&[("user", "hello")]);
        assert!(scrub_conversation(&scrubber, &keyed(), &mut body).is_err());
    }

    #[test]
    fn debug_output_hides_what_was_replaced() {
        let mut body = chat(&[("user", "Ask Imogen")]);
        let replaced = scrub_conversation(&scrubber(&["Imogen"]), &keyed(), &mut body).unwrap();
        assert_eq!(format!("{replaced:?}"), "Replaced(1 hidden)");
    }
}
