//! The PII scrubber (the `PiiScrubber` slot, chunk 3): finds what identifies the user in a message,
//! so it can be replaced before the message leaves and put back in the reply. Built from parts:
//! patterns for structured details, entity recognition for names, places and organisations, a
//! context pass for relationships (slice 6). Credentials are not its business: the credential
//! scanner in the always-on floor finds those, and they are never put back.

mod conversation;
mod entities;
mod patterns;
mod restore;
pub mod surrogate;

use std::fmt;
use std::ops::Range;
use std::path::Path;

use bullbar_core::levels::ScrubberGrade;

pub use conversation::{Replaced, scrub_conversation, scrub_text};
pub use entities::{Entities, EntityError};
pub use patterns::Patterns;
pub use restore::{Restorer, Streaming};

/// What kind of personal detail a span is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PiiKind {
    /// A private person's name, whole or part.
    Person,
    /// A town, city, region or venue tied to the writer.
    Place,
    /// A street address, flat number or postcode.
    Address,
    /// An employer, school, clinic or club tied to the writer.
    Organisation,
    /// A specific calendar date.
    Date,
    /// An email address.
    Email,
    /// A phone number.
    Phone,
    /// An account, IBAN, passport, patient, membership or similar number.
    Id,
    /// A phrase linking someone to the writer ("my wife").
    Relationship,
}

impl PiiKind {
    /// The kind's name in a placeholder (`[PHONE_1]`).
    pub fn label(self) -> &'static str {
        match self {
            PiiKind::Person => "PERSON",
            PiiKind::Place => "PLACE",
            PiiKind::Address => "ADDRESS",
            PiiKind::Organisation => "ORGANISATION",
            PiiKind::Date => "DATE",
            PiiKind::Email => "EMAIL",
            PiiKind::Phone => "PHONE",
            PiiKind::Id => "ID",
            PiiKind::Relationship => "RELATIONSHIP",
        }
    }
}

impl fmt::Display for PiiKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            PiiKind::Person => "a name",
            PiiKind::Place => "a place",
            PiiKind::Address => "an address",
            PiiKind::Organisation => "an organisation",
            PiiKind::Date => "a date",
            PiiKind::Email => "an email address",
            PiiKind::Phone => "a phone number",
            PiiKind::Id => "an identifying number",
            PiiKind::Relationship => "a relationship",
        })
    }
}

/// One detail found in a text: where it is (byte offsets) and what it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// Where in the text.
    pub range: Range<usize>,
    /// What kind of detail.
    pub kind: PiiKind,
}

/// Why the scrubber couldn't be built or couldn't finish. Either way nothing may leave (rule 4).
#[derive(Debug, thiserror::Error)]
pub enum ScrubError {
    /// A pattern didn't compile.
    #[error("compiling the scrubber's patterns: {0}")]
    Patterns(#[from] regex::Error),
    /// The entity part failed.
    #[error(transparent)]
    Entities(#[from] EntityError),
    /// A route needed scrubbing and there was no scrubber.
    #[error("there is no scrubber")]
    Missing,
}

/// What finds names, places and organisations: the entity model, or a stand-in in tests.
pub trait Detector: Send + Sync {
    /// Every entity in `text`. An error means the text wasn't checked.
    fn find(&self, text: &str) -> Result<Vec<Found>, EntityError>;
}

impl Detector for Entities {
    fn find(&self, text: &str) -> Result<Vec<Found>, EntityError> {
        Entities::find(self, text)
    }
}

/// The scrubber, from its parts.
pub struct Scrubber {
    patterns: Patterns,
    entities: Option<Box<dyn Detector>>,
}

impl Scrubber {
    /// The pattern part alone: grade `patterns`, which counts as no scrubber.
    pub fn patterns_only() -> Result<Self, ScrubError> {
        Ok(Scrubber {
            patterns: Patterns::new()?,
            entities: None,
        })
    }

    /// Patterns and the entity model from `model_dir`: grade `entities`. A model that won't load is
    /// an error, never a quiet fall back to patterns.
    pub fn with_entities(model_dir: &Path) -> Result<Self, ScrubError> {
        Self::with_detector(Box::new(Entities::load(model_dir)?))
    }

    /// Patterns and another detector at the entity part's grade: for tests, which can't load the
    /// model.
    pub fn with_detector(detector: Box<dyn Detector>) -> Result<Self, ScrubError> {
        Ok(Scrubber {
            patterns: Patterns::new()?,
            entities: Some(detector),
        })
    }

    /// The grade of the parts actually built (ADR 0020, point 6).
    pub fn grade(&self) -> ScrubberGrade {
        match self.entities {
            Some(_) => ScrubberGrade::Entities,
            None => ScrubberGrade::Patterns,
        }
    }

    /// Every detail found in `text`, sorted, overlaps merged. Where a pattern and the model find
    /// overlapping spans, they become one span with the pattern's kind (patterns are exact about
    /// shape). An error means the text wasn't fully checked.
    pub fn find(&self, text: &str) -> Result<Vec<Found>, ScrubError> {
        let mut spans: Vec<(Found, u8)> = self
            .patterns
            .find(text)
            .into_iter()
            .map(|f| (f, 0))
            .collect();
        if let Some(entities) = &self.entities {
            spans.extend(entities.find(text)?.into_iter().map(|f| (f, 1)));
        }
        spans.sort_by_key(|(f, rank)| (f.range.start, std::cmp::Reverse(f.range.end), *rank));
        let mut merged: Vec<(Found, u8)> = Vec::new();
        for (found, rank) in spans {
            match merged.last_mut() {
                Some((last, last_rank)) if found.range.start < last.range.end => {
                    if rank < *last_rank {
                        *last_rank = rank;
                        last.kind = found.kind;
                    }
                    last.range.end = last.range.end.max(found.range.end);
                }
                _ => merged.push((found, rank)),
            }
        }
        Ok(merged.into_iter().map(|(f, _)| f).collect())
    }
}

/// A pattern for `s` that matches any run of whitespace between its words. Providers write some
/// spaces as other characters (a narrow no-break space, U+202F, in the chunk 3 hand check), and a
/// name with one between its words is still the name.
pub(crate) fn spaced_pattern(s: &str) -> String {
    s.split_whitespace()
        .map(regex::escape)
        .collect::<Vec<_>>()
        .join(r"\s+")
}

/// `s` lowercased, each run of whitespace one plain space: for comparing details.
pub(crate) fn squashed(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}
