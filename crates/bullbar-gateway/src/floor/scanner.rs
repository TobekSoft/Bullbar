//! The credential scanner (the `CredentialScanner` slot, Rust-native): finds keys, tokens,
//! passwords and card numbers, and replaces them with numbered placeholders when a route strips
//! them (ADR 0009). It errs towards finding too much: a false find keeps a request local and says
//! why; a miss would send a credential out, where only the outbound check stands behind it.

use std::collections::BTreeSet;
use std::ops::Range;

use regex::Regex;
use serde_json::Value;

use super::{Stripped, each_string, each_string_mut, placeholder};

/// How a rule's matches become credentials.
enum Kind {
    /// The whole match is the credential.
    Whole,
    /// Only the first group is (the value after `password =`, say).
    Group,
    /// A run of digits that is a card number only if the Luhn check passes.
    Card,
    /// A long token that counts only if it looks random (mixed case and digits, high entropy).
    Random,
}

/// The scanner's rule set: each rule with what it finds, as told to the user.
pub struct Scanner {
    rules: Vec<(Kind, &'static str, Regex)>,
}

impl Scanner {
    /// Compiles the rules.
    pub fn new() -> Result<Self, regex::Error> {
        let rules = [
            // Private key blocks; an unterminated one runs to the end of the text.
            (
                Kind::Whole,
                "a private key",
                r"(?s)-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----.*?(?:-----END [A-Z0-9 ]*PRIVATE KEY-----|\z)",
            ),
            // Tokens recognisable by their prefix.
            (
                Kind::Whole,
                "a token",
                r"\b(?:AKIA[0-9A-Z]{16}|gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{30,}|xox[abprs]-[A-Za-z0-9-]{10,}|AIza[0-9A-Za-z_-]{35}|glpat-[A-Za-z0-9_-]{20,}|hf_[A-Za-z0-9]{30,}|[sr]k[-_][A-Za-z0-9_-]{20,})",
            ),
            // Signed web tokens: three base64url parts, the first two JSON.
            (
                Kind::Whole,
                "a signed token",
                r"\beyJ[A-Za-z0-9_-]{8,}\.eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}",
            ),
            // Authorisation headers pasted with a request.
            (
                Kind::Group,
                "an authorisation value",
                r"(?i)\b(?:bearer|basic|token)\s+([A-Za-z0-9._~+/-]{16,}=*)",
            ),
            // `password = ...`, `"api_key": "..."` and the like, in config files and code.
            (
                Kind::Group,
                "a password or key",
                r#"(?i)(?:password|passwd|pwd|passphrase|secret|api[_-]?key|access[_-]?key|auth[_-]?token|access[_-]?token|client[_-]?secret|private[_-]?key)["']?\s*[:=]\s*["']?([^\s"',;\[\]]{6,})"#,
            ),
            // A password in a URL: scheme://user:password@host.
            (
                Kind::Group,
                "a password in a URL",
                r"[A-Za-z][A-Za-z0-9+.-]*://[^\s:/@]+:([^\s@/]+)@",
            ),
            (Kind::Card, "a card number", r"\b(?:\d[ -]?){12,18}\d\b"),
            (
                Kind::Random,
                "a random-looking token",
                r"[A-Za-z0-9+/_-]{32,}={0,2}",
            ),
        ];
        let rules = rules
            .into_iter()
            .map(|(kind, label, pattern)| Ok((kind, label, Regex::new(pattern)?)))
            .collect::<Result<_, regex::Error>>()?;
        Ok(Scanner { rules })
    }

    /// Where the credentials are in `text`, and what each is, sorted, overlaps merged (keeping the
    /// first rule's label).
    pub fn spans(&self, text: &str) -> Vec<(Range<usize>, &'static str)> {
        let mut spans: Vec<(Range<usize>, &'static str)> = Vec::new();
        for (kind, label, regex) in &self.rules {
            for caps in regex.captures_iter(text) {
                let found = match kind {
                    Kind::Group => caps.get(1),
                    Kind::Whole | Kind::Card | Kind::Random => caps.get(0),
                };
                let Some(m) = found else { continue };
                let keep = match kind {
                    Kind::Card => luhn(m.as_str()),
                    Kind::Random => looks_random(m.as_str()),
                    Kind::Whole | Kind::Group => true,
                };
                if keep {
                    spans.push((m.range(), *label));
                }
            }
        }
        spans.sort_by_key(|(r, _)| (r.start, r.end));
        let mut merged: Vec<(Range<usize>, &'static str)> = Vec::new();
        for (span, label) in spans {
            match merged.last_mut() {
                Some((last, _)) if span.start <= last.end => last.end = last.end.max(span.end),
                _ => merged.push((span, label)),
            }
        }
        merged
    }

    /// Whether any string in the body, keys included, holds a credential.
    pub fn found(&self, body: &Value) -> bool {
        let mut found = false;
        each_string(body, &mut |s| found = found || !self.spans(s).is_empty());
        found
    }

    /// Replaces every credential in the body's string values with its placeholder. Keys aren't
    /// changed; a credential in a key is left for the outbound check to stop.
    pub fn strip(&self, body: &mut Value) -> Stripped {
        let mut values: Vec<(String, &'static str)> = Vec::new();
        each_string_mut(body, &mut |s| {
            let spans = self.spans(s);
            if spans.is_empty() {
                return;
            }
            let mut out = String::with_capacity(s.len());
            let mut at = 0;
            for (span, label) in spans {
                let value = &s[span.clone()];
                let n = match values.iter().position(|(v, _)| v == value) {
                    Some(i) => i + 1,
                    None => {
                        values.push((value.to_owned(), label));
                        values.len()
                    }
                };
                out.push_str(&s[at..span.start]);
                out.push_str(&placeholder(n));
                at = span.end;
            }
            out.push_str(&s[at..]);
            *s = out;
        });
        Stripped(values)
    }
}

/// The Luhn check over the digits in `text`, for 13 to 19 digits.
fn luhn(text: &str) -> bool {
    let digits: Vec<u32> = text.chars().filter_map(|c| c.to_digit(10)).collect();
    if !(13..=19).contains(&digits.len()) {
        return false;
    }
    let sum: u32 = digits
        .iter()
        .rev()
        .enumerate()
        .map(|(i, &d)| match (i % 2, d * 2) {
            (0, _) => d,
            (_, doubled) if doubled > 9 => doubled - 9,
            (_, doubled) => doubled,
        })
        .sum();
    sum.is_multiple_of(10)
}

/// A long token that mixes upper case, lower case and digits with high entropy. Longer than 200
/// characters is data (an embedded image, say), not a credential.
fn looks_random(token: &str) -> bool {
    let token = token.trim_end_matches('=');
    if token.len() > 200 {
        return false;
    }
    let has = |f: fn(&char) -> bool| token.chars().any(|c| f(&c));
    if !(has(char::is_ascii_uppercase)
        && has(char::is_ascii_lowercase)
        && has(char::is_ascii_digit))
    {
        return false;
    }
    let distinct: BTreeSet<char> = token.chars().collect();
    let len = token.len() as f64;
    let entropy: f64 = distinct
        .iter()
        .map(|&c| {
            let p = token.chars().filter(|&x| x == c).count() as f64 / len;
            -p * p.log2()
        })
        .sum();
    entropy >= 4.2
}

#[cfg(test)]
mod tests {
    use super::super::FAKES;
    use super::*;

    fn found(text: &str) -> Vec<&str> {
        Scanner::new()
            .unwrap()
            .spans(text)
            .into_iter()
            .map(|(r, _)| &text[r])
            .collect()
    }

    #[test]
    fn every_kind_of_fake_is_found_whole() {
        for fake in FAKES {
            assert_eq!(found(&format!("see: {fake} here")), [*fake], "{fake}");
        }
    }

    #[test]
    fn only_the_value_of_an_assignment_is_taken() {
        assert_eq!(found("password = fake-hunter2"), ["fake-hunter2"]);
        assert_eq!(found(r#"{"api_key": "fakefakefake"}"#), ["fakefakefake"]);
        assert_eq!(
            found("Authorization: Bearer fakeFAKEfake0123"),
            ["fakeFAKEfake0123"]
        );
        assert_eq!(found("https://user:fakepass@example.com/"), ["fakepass"]);
    }

    #[test]
    fn a_random_looking_token_is_found() {
        assert_eq!(
            found("token is q8Zr2LmX0vTb7NcK4yWp1HdJ6sFg9AeU"),
            ["q8Zr2LmX0vTb7NcK4yWp1HdJ6sFg9AeU"]
        );
    }

    #[test]
    fn ordinary_text_is_left_alone() {
        for text in [
            "what is six times seven?",
            "commit 3f2a9c1d8e7b6a5f4e3d2c1b0a9f8e7d6c5b4a39 fixed it",
            "id 123e4567-e89b-12d3-a456-426614174000",
            "call getUserAccountInformationById please",
            "the number 1234 5678 9012 3456 fails the check",
            "max_tokens: 500",
            "password = [CREDENTIAL_1]",
        ] {
            assert!(found(text).is_empty(), "{text}: {:?}", found(text));
        }
    }

    #[test]
    fn long_data_is_not_a_credential() {
        let data = "aB3".repeat(100);
        assert!(found(&data).is_empty());
    }

    #[test]
    fn an_unterminated_key_block_runs_to_the_end() {
        let text = "here:\n-----BEGIN PRIVATE KEY-----\nRkFLRQ==\nmore";
        assert_eq!(found(text), [&text[6..]]);
    }
}
