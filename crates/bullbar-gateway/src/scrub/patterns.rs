//! The scrubber's pattern part: structured details that have a shape (emails, phone numbers,
//! identifiers, dates, addresses), found the way the credential scanner finds keys. Deterministic,
//! fast and language-light; names, places and organisations need entity recognition instead.
//!
//! It leans towards finding: a false find costs a placeholder in a message that is being scrubbed
//! anyway (only requests rated Personal or above are), while a miss sends a detail out.

use std::ops::Range;

use regex::Regex;

use super::{Found, PiiKind};

const MONTHS: &str = "January|February|March|April|May|June|July|August|September|October|November|December|Jan|Feb|Mar|Apr|Jun|Jul|Aug|Sep|Sept|Oct|Nov|Dec";

const STREET: &str = "Road|Street|Lane|Avenue|Close|Court|Drive|Way|Mews|Place|Crescent|Gardens|Terrace|Square|Row|Grove|Walk|Rise|Park|Rd|St|Ave";

/// The pattern rules, most specific first: where two overlap, the first rule's kind is kept.
pub struct Patterns {
    rules: Vec<(PiiKind, Regex)>,
}

impl Patterns {
    /// Compiles the rules.
    pub fn new() -> Result<Self, regex::Error> {
        let rules: Vec<(PiiKind, String)> = vec![
            (
                PiiKind::Email,
                r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}\b".into(),
            ),
            // IBANs: two letters, two check digits, then groups of four.
            (
                PiiKind::Id,
                r"\b[A-Z]{2}\d{2}(?:[ ]?[A-Z0-9]{4}){2,7}(?:[ ]?[A-Z0-9]{1,3})?\b".into(),
            ),
            // UK national insurance numbers (QQ 12 34 56 C).
            (
                PiiKind::Id,
                r"\b[A-Z]{2}[ ]?\d{2}[ ]?\d{2}[ ]?\d{2}[ ]?[A-D]\b".into(),
            ),
            // Dates written out: 14 March 1987, 22nd of August, March 14, 1987.
            (
                PiiKind::Date,
                format!(
                    r"\b\d{{1,2}}(?:st|nd|rd|th)?(?:\s+of)?\s+(?:{MONTHS})\b(?:,?\s+(?:19|20)\d{{2}}\b)?"
                ),
            ),
            (
                PiiKind::Date,
                format!(
                    r"\b(?:{MONTHS})\s+\d{{1,2}}(?:st|nd|rd|th)?\b(?:,?\s+(?:19|20)\d{{2}}\b)?"
                ),
            ),
            // Dates in figures: 2026-12-03, 09/04/2027, 02.11.1990.
            (PiiKind::Date, r"\b(?:19|20)\d{2}-\d{2}-\d{2}\b".into()),
            (
                PiiKind::Date,
                r"\b\d{1,2}[/.]\d{1,2}[/.](?:19|20)?\d{2}\b".into(),
            ),
            // Phone numbers: international, UK mobiles and landlines, North American.
            (
                PiiKind::Phone,
                r"\+\d{1,3}[ -]?\(?\d{1,4}\)?(?:[ -]?\d{2,4}){2,4}\b".into(),
            ),
            (PiiKind::Phone, r"\b0\d{3,4}[ ]?\d{3}[ ]?\d{3,4}\b".into()),
            (
                PiiKind::Phone,
                r"(?:\(\d{3}\)[ ]?|\b\d{3}[-. ])?\b\d{3}-\d{4}\b".into(),
            ),
            // NHS-style numbers (999 000 1234), sort codes (00-00-00).
            (PiiKind::Id, r"\b\d{3} \d{3} \d{4}\b".into()),
            (PiiKind::Id, r"\b\d{2}-\d{2}-\d{2}\b".into()),
            // Reference numbers with a prefix: GRC-00417, RIE-4471902.
            (PiiKind::Id, r"\b[A-Z]{2,5}-\d{5,}\b".into()),
            // Long runs of digits: account, passport and similar numbers.
            (PiiKind::Id, r"\b\d{8,12}\b".into()),
            // Postcodes: UK (M20 9ZZ), US state and ZIP (IL 62704).
            (
                PiiKind::Address,
                r"\b[A-Z]{1,2}\d[A-Z\d]?[ ]?\d[A-Z]{2}\b".into(),
            ),
            (PiiKind::Address, r"\b[A-Z]{2} \d{5}(?:-\d{4})?\b".into()),
            // Street addresses: 14 Fakeley Road, 3B Gloamwick Court, Mardlebury Street.
            (
                PiiKind::Address,
                format!(r"\b(?:\d+[A-Za-z]?\s+)?(?:[A-Z][a-z]+\s+){{1,3}}(?:{STREET})\b"),
            ),
            // Flat and unit numbers.
            (
                PiiKind::Address,
                r"(?i)\b(?:flat|apartment|apt\.?|unit)\s+\d+[a-z]?\b".into(),
            ),
        ];
        let rules = rules
            .into_iter()
            .map(|(kind, pattern)| Ok((kind, Regex::new(&pattern)?)))
            .collect::<Result<_, regex::Error>>()?;
        Ok(Patterns { rules })
    }

    /// Every match in `text`, sorted, overlaps merged (keeping the earlier rule's kind).
    pub fn find(&self, text: &str) -> Vec<Found> {
        let mut spans: Vec<(Range<usize>, usize, PiiKind)> = Vec::new();
        for (order, (kind, regex)) in self.rules.iter().enumerate() {
            for m in regex.find_iter(text) {
                spans.push((m.range(), order, *kind));
            }
        }
        spans.sort_by_key(|(r, order, _)| (r.start, std::cmp::Reverse(r.end), *order));
        let mut merged: Vec<(Range<usize>, usize, PiiKind)> = Vec::new();
        for (range, order, kind) in spans {
            match merged.last_mut() {
                Some((last, last_order, last_kind)) if range.start < last.end => {
                    if order < *last_order {
                        *last_order = order;
                        *last_kind = kind;
                    }
                    last.end = last.end.max(range.end);
                }
                _ => merged.push((range, order, kind)),
            }
        }
        merged
            .into_iter()
            .map(|(range, _, kind)| Found { range, kind })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn everyday_numbers_are_not_details() {
        let patterns = Patterns::new().unwrap();
        for text in [
            "Boil a 6-minute egg at 100°C for 12:30 lunch.",
            "Explain compound interest using £1,000 at 5% over 10 years.",
            "Rust 1.92.0 came out in 2025; regex is at 1.13.1.",
            "Dates in ISO 8601 look like YYYY-MM-DD.",
            "Read chapters 3 to 7, pages 120-145, by Friday.",
            "The ratio was 16:9 and the score 3-2.",
            "Use port 8480 and the address 127.0.0.1.",
            "Section 4.2.1 of RFC 9110 covers it.",
        ] {
            let found: Vec<_> = patterns
                .find(text)
                .into_iter()
                .map(|f| (f.kind, &text[f.range]))
                .collect();
            assert!(found.is_empty(), "{text}: {found:?}");
        }
    }

    #[test]
    fn overlapping_rules_keep_the_more_specific_kind() {
        let patterns = Patterns::new().unwrap();
        let text = "Pay into GB82 WEST 1234 5698 7654 32 today.";
        let found = patterns.find(text);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, PiiKind::Id);
        assert_eq!(&text[found[0].range.clone()], "GB82 WEST 1234 5698 7654 32");
    }
}
