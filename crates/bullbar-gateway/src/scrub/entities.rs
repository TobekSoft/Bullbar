//! The scrubber's entity part: names, places and organisations, which have no shape a pattern can
//! find. A GLiNER model (`gliner_multi_pii-v1`, Apache-2.0) run in-process through ONNX Runtime, on
//! the CPU, nothing sent anywhere (ADR 0020, slice 0 chose it by measurement).
//!
//! The model is a folder holding `tokenizer.json` and `model.onnx`; it is not in the repository
//! (1.16 GB). A model that fails to load, or fails on a message, is an error the caller must treat as
//! a block (rule 4), never as "nothing found".

use std::ops::Range;
use std::path::Path;

use gliner::model::GLiNER;
use gliner::model::input::text::TextInput;
use gliner::model::params::Parameters;
use gliner::model::pipeline::span::SpanMode;
use orp::params::RuntimeParameters;
use regex::Regex;

use super::{Found, PiiKind};

/// What the model is asked for, and the kind each answer becomes.
const LABELS: &[(&str, PiiKind)] = &[
    ("person", PiiKind::Person),
    ("location", PiiKind::Place),
    ("street address", PiiKind::Address),
    ("postal code", PiiKind::Address),
    ("organization", PiiKind::Organisation),
    ("date", PiiKind::Date),
    ("email", PiiKind::Email),
    ("phone number", PiiKind::Phone),
    ("bank account number", PiiKind::Id),
    ("identification number", PiiKind::Id),
];

/// The score a span needs (measured in slice 0: 0.5 found 86%; 0.3 found 88% with four times the
/// false finds).
const THRESHOLD: f32 = 0.5;

/// How much of a text the model reads at once, in the library's own words (a word or a punctuation
/// mark). The library reads 512 and silently drops the rest; windows stay well under that, since the
/// model's own limit is in sub-word pieces and long or rare words split into several.
const WINDOW: usize = 200;

/// How far windows overlap, so a detail cut by one window's edge is whole in the next.
const OVERLAP: usize = 50;

/// The library's word splitter (gline-rs `RegexSplitter::default`), to count words as it does.
const WORD: &str = r"\w+(?:[-_]\w+)*|\S";

/// Why the entity part failed.
#[derive(Debug, thiserror::Error)]
pub enum EntityError {
    /// The model or its tokenizer couldn't be loaded.
    #[error("loading the entity model from {path}: {reason}")]
    Load {
        /// The model folder.
        path: String,
        /// What went wrong.
        reason: String,
    },
    /// The model failed on a message.
    #[error("the entity model failed on a message: {0}")]
    Inference(String),
}

/// The loaded model.
pub struct Entities {
    model: GLiNER<SpanMode>,
    word: Regex,
}

impl Entities {
    /// Loads the model from a folder holding `tokenizer.json` and `model.onnx` (a few seconds).
    pub fn load(dir: &Path) -> Result<Self, EntityError> {
        let load = |reason: String| EntityError::Load {
            path: dir.display().to_string(),
            reason,
        };
        let tokenizer = dir.join("tokenizer.json");
        let model = dir.join("model.onnx");
        for file in [&tokenizer, &model] {
            if !file.is_file() {
                return Err(load(format!("{} is missing", file.display())));
            }
        }
        let model = GLiNER::<SpanMode>::new(
            Parameters::default().with_threshold(THRESHOLD),
            RuntimeParameters::default(),
            &tokenizer,
            &model,
        )
        .map_err(|e| load(e.to_string()))?;
        let word = Regex::new(WORD).map_err(|e| load(e.to_string()))?;
        Ok(Entities { model, word })
    }

    /// Every entity in `text` that names something private, filtered (see [`worth_keeping`]). A long
    /// text is read in overlapping windows, to its end.
    pub fn find(&self, text: &str) -> Result<Vec<Found>, EntityError> {
        let mut found: Vec<Found> = Vec::new();
        for window in windows(&self.word, text, WINDOW, OVERLAP) {
            for f in self.find_in(&text[window.clone()])? {
                let range = f.range.start + window.start..f.range.end + window.start;
                if !found.iter().any(|g| g.range == range && g.kind == f.kind) {
                    found.push(Found {
                        range,
                        kind: f.kind,
                    });
                }
            }
        }
        Ok(found)
    }

    /// Entities in a text short enough for the model to read whole.
    fn find_in(&self, text: &str) -> Result<Vec<Found>, EntityError> {
        let labels: Vec<&str> = LABELS.iter().map(|(label, _)| *label).collect();
        let input = TextInput::from_str(&[text], &labels)
            .map_err(|e| EntityError::Inference(e.to_string()))?;
        let output = self
            .model
            .inference(input)
            .map_err(|e| EntityError::Inference(e.to_string()))?;
        let mut found = Vec::new();
        for span in output.spans.iter().flatten() {
            let Some(kind) = LABELS
                .iter()
                .find(|(label, _)| *label == span.class())
                .map(|(_, kind)| *kind)
            else {
                continue;
            };
            let Some(range) = locate(text, span.offsets(), span.text()) else {
                continue;
            };
            if worth_keeping(&text[range.clone()], kind) {
                found.push(Found { range, kind });
            }
        }
        Ok(found)
    }
}

/// Byte ranges of `text` covering it whole, each at most `size` words, each starting `size - overlap`
/// words after the last. A text that fits is one window; a text with no words has none (the model
/// fails on one, and there is nothing in it to find).
fn windows(word: &Regex, text: &str, size: usize, overlap: usize) -> Vec<Range<usize>> {
    let words: Vec<Range<usize>> = word.find_iter(text).map(|m| m.range()).collect();
    if words.is_empty() {
        return Vec::new();
    }
    if words.len() <= size {
        return std::iter::once(0..text.len()).collect();
    }
    let step = size.saturating_sub(overlap).max(1);
    let mut out = Vec::new();
    let mut first = 0;
    loop {
        let last = (first + size).min(words.len()) - 1;
        let start = if first == 0 { 0 } else { words[first].start };
        let end = if last == words.len() - 1 {
            text.len()
        } else {
            words[last].end
        };
        out.push(start..end);
        if last == words.len() - 1 {
            return out;
        }
        first += step;
    }
}

/// Where the span is in `text`: the model's offsets if they hold its text, else the first place
/// the text appears. A span whose text isn't in the message at all is dropped: there's nothing there
/// to replace.
fn locate(text: &str, (start, end): (usize, usize), said: &str) -> Option<Range<usize>> {
    if text.get(start..end) == Some(said) {
        return Some(start..end);
    }
    text.find(said).map(|at| at..at + said.len())
}

/// Whether a span the model found is a detail rather than an ordinary word. The model also tags
/// common nouns ("school", "bank", "council"), pronouns and lone weekdays or months; replacing those
/// would garble the message and protect nothing. Kept: anything with a capital letter or a digit,
/// except the words listed below, and people in lowercase too: people type names in lowercase
/// ("ask sam"), and the model tags relationships ("my wife") as people. Measured on the synthetic
/// set: keeping lowercase people found 6 more details for 3 more false finds. A lowercase person of
/// two words or more is kept only as a relationship: the model also tags phrases such as
/// "professional tool caller" (2026-11-17, chunk 3's hand check).
fn worth_keeping(span: &str, kind: PiiKind) -> bool {
    let span = span.trim();
    if span.chars().count() < 2 {
        return false;
    }
    let shaped = span.chars().any(|c| c.is_uppercase() || c.is_ascii_digit());
    if kind != PiiKind::Person && !shaped {
        return false;
    }
    let lower = span.to_lowercase();
    if kind == PiiKind::Person
        && !shaped
        && lower.split_whitespace().count() > 1
        && !is_relationship(&lower)
    {
        return false;
    }
    if kind == PiiKind::Date && CALENDAR_WORDS.contains(&lower.as_str()) {
        return false;
    }
    !ORDINARY.contains(&lower.as_str())
}

/// Whether a phrase (lowercased) links someone to the writer or another person: "my wife", "her
/// teacher". The model tags these as people; a fake name in their place would change what the
/// message says, so they get a token instead.
pub(super) fn is_relationship(lower: &str) -> bool {
    ["my ", "his ", "her ", "our ", "their ", "your "]
        .iter()
        .any(|p| lower.starts_with(p))
}

/// Weekdays and months alone: not a specific date. A full date ("3 June") is found whole.
const CALENDAR_WORDS: &[&str] = &[
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
    "today",
    "tomorrow",
    "yesterday",
];

/// Words the model tags that identify no one: pronouns (it tags "she" as a person, and a lowercase
/// person is kept, so "she" was replaced by a name: found in the chunk 3 hand check), forms of
/// address, and chat roles, which system prompts name all the time ("answer the user's questions").
/// A role word replaced in the text but left in a message's `role` field would block every request
/// (also found in the hand check).
const ORDINARY: &[&str] = &[
    "i",
    "i'm",
    "me",
    "my",
    "myself",
    "we",
    "us",
    "our",
    "ourselves",
    "you",
    "your",
    "yourself",
    "he",
    "him",
    "his",
    "himself",
    "she",
    "her",
    "hers",
    "herself",
    "they",
    "them",
    "their",
    "theirs",
    "themselves",
    "it",
    "its",
    "someone",
    "somebody",
    "anyone",
    "anybody",
    "everyone",
    "everybody",
    "nobody",
    "mum",
    "dad",
    "sir",
    "madam",
    "user",
    "assistant",
    "system",
    "developer",
    "tool",
    "human",
    "ai",
    "bot",
    "chatbot",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_words_are_dropped_and_details_kept() {
        for (span, kind) in [
            ("school", PiiKind::Organisation),
            ("bank", PiiKind::Organisation),
            ("I", PiiKind::Person),
            ("Thursday", PiiKind::Date),
            ("June", PiiKind::Date),
            ("Mum", PiiKind::Person),
            ("We", PiiKind::Person),
            ("council", PiiKind::Organisation),
            // Chat roles, which system prompts name all the time (found in the chunk 3 hand check:
            // "the user" scrubbed, then found in a message's role on the way out).
            ("user", PiiKind::Person),
            ("User", PiiKind::Person),
            ("assistant", PiiKind::Person),
            ("system", PiiKind::Person),
            // Pronouns, which the model tags as people ("did I say she lives on" left as "did I
            // say tom lives on": found in the chunk 3 hand check).
            ("she", PiiKind::Person),
            ("She", PiiKind::Person),
            ("he", PiiKind::Person),
            ("her", PiiKind::Person),
            ("him", PiiKind::Person),
            ("they", PiiKind::Person),
            ("them", PiiKind::Person),
            ("herself", PiiKind::Person),
            ("someone", PiiKind::Person),
        ] {
            assert!(!worth_keeping(span, kind), "{span}");
        }
        for (span, kind) in [
            ("Imogen", PiiKind::Person),
            ("Fakeley Primary School", PiiKind::Organisation),
            ("3 June", PiiKind::Date),
            ("June", PiiKind::Person),
            ("sam", PiiKind::Person),
            ("my wife", PiiKind::Person),
            ("Tobermory", PiiKind::Place),
        ] {
            assert!(worth_keeping(span, kind), "{span}");
        }
    }

    /// A lowercase phrase of two or more words is a person only as a relationship ("my wife"). The
    /// model tagged "professional tool caller" in a chat client's system prompt as a person; it left
    /// as a fake name and its words were replaced everywhere as name parts, garbling the prompt
    /// (found in the chunk 3 hand check). A lowercase name of one word ("ask sam") is still kept.
    #[test]
    fn a_lowercase_phrase_is_a_person_only_as_a_relationship() {
        for span in ["professional tool caller", "boiler engineer", "two kids"] {
            assert!(!worth_keeping(span, PiiKind::Person), "{span}");
        }
        for span in [
            "my wife",
            "our landlord",
            "her teacher",
            "my two kids",
            "sam",
            "Kwame Fakeson",
        ] {
            assert!(worth_keeping(span, PiiKind::Person), "{span}");
        }
    }

    #[test]
    fn a_text_without_words_has_no_windows() {
        // The model fails on a text with no words (seen 2026-11-17: an empty message from a chat
        // client blocked the request), so it must never be given one.
        let word = Regex::new(WORD).unwrap();
        assert!(windows(&word, "", 5, 2).is_empty());
        assert!(windows(&word, " \n\t ", 5, 2).is_empty());
        assert_eq!(windows(&word, "?", 5, 2), vec![0..1]);
    }

    #[test]
    fn windows_cover_a_long_text_whole_and_overlap() {
        let word = Regex::new(WORD).unwrap();
        let whole = windows(&word, "short text.", 5, 2);
        assert_eq!((whole.len(), whole.first().cloned()), (1, Some(0..11)));
        let text = "a b c d e f g h i j k";
        let got: Vec<&str> = windows(&word, text, 4, 2)
            .into_iter()
            .map(|r| &text[r])
            .collect();
        assert_eq!(got, ["a b c d", "c d e f", "e f g h", "g h i j", "i j k"]);
        // Punctuation counts as a word, as the library counts it.
        assert_eq!(windows(&word, "a, b, c.", 3, 1).len(), 3);
        // Every byte is in some window.
        let long = "Imogen Fakeworth, of 12 Fakery Lane. ".repeat(300);
        let ws = windows(&word, &long, WINDOW, OVERLAP);
        assert_eq!(ws[0].start, 0);
        assert_eq!(ws.last().map(|w| w.end), Some(long.len()));
        assert!(
            ws.windows(2).all(|p| p[1].start < p[0].end),
            "windows overlap"
        );
    }

    #[test]
    fn a_span_is_placed_by_its_text_when_the_offsets_are_off() {
        let text = "Ask Priya about it.";
        assert_eq!(locate(text, (4, 9), "Priya"), Some(4..9));
        assert_eq!(locate(text, (0, 5), "Priya"), Some(4..9));
        assert_eq!(locate(text, (4, 9), "Kwame"), None);
    }

    #[test]
    fn a_missing_model_is_an_error_not_an_empty_part() {
        let err = Entities::load(Path::new("/nonexistent/bullbar-model"))
            .err()
            .map(|e| e.to_string());
        assert!(err.is_some_and(|e| e.contains("tokenizer.json is missing")));
    }
}
