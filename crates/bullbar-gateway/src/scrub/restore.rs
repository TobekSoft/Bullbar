//! Restoring a reply (ADR 0020, point 8): each surrogate in the answer is put back to the detail it
//! stood for, in any case ("noah" back to "imogen"), whole words only. What can't be matched exactly
//! is never guessed at: a surrogate that never came back, a token that came back changed, and a
//! token Bullbar never sent are named in the footer and left as they are.
//!
//! Credential placeholders are not in the map and are never restored (ADR 0009).

use regex::{Regex, RegexBuilder};

use super::{PiiKind, Replaced, spaced_pattern, squashed};

/// The longest a token can be, `[ORGANISATION_99]` and some: how far back an unclosed `[` is held
/// in a stream.
const TOKEN_HOLD: usize = 24;

/// Puts surrogates back.
pub struct Restorer {
    /// (surrogate, original, kind), in the order the details appeared in the request.
    map: Vec<(String, String, PiiKind)>,
    /// Per map entry: in the latest user message, so expected back in the answer.
    latest: Vec<bool>,
    finder: Option<Regex>,
    /// Token-like text: `[PHONE_1]`, `PHONE_1`, `[phone 1]`. None when there is nothing to restore.
    token_like: Option<Regex>,
    longest: usize,
}

impl Restorer {
    /// A restorer for what was replaced in the request.
    pub fn new(replaced: &Replaced) -> Result<Self, regex::Error> {
        let map: Vec<(String, String, PiiKind)> = replaced
            .entries()
            .map(|(kind, original, surrogate)| (surrogate.to_owned(), original.to_owned(), kind))
            .collect();
        let latest = map
            .iter()
            .map(|(_, original, _)| replaced.in_latest(original))
            .collect();
        let finder = if map.is_empty() {
            None
        } else {
            // Longest first, so "Noah Silva" wins over "Noah" where both match.
            let mut surrogates: Vec<&str> = map.iter().map(|(s, _, _)| s.as_str()).collect();
            surrogates.sort_by_key(|s| std::cmp::Reverse(s.len()));
            let alternation: Vec<String> = surrogates.iter().map(|s| spaced_pattern(s)).collect();
            Some(
                RegexBuilder::new(&alternation.join("|"))
                    .case_insensitive(true)
                    .build()?,
            )
        };
        let token_like = Some(Regex::new(r"(?i)\[?\b([A-Z]+)[_ ](\d+)\b\]?")?);
        let longest = map
            .iter()
            .map(|(s, _, _)| s.chars().count())
            .max()
            .unwrap_or(0);
        Ok(Restorer {
            map,
            latest,
            finder,
            token_like,
            longest,
        })
    }

    /// A restorer with nothing to put back (a request nothing was replaced in).
    pub fn none() -> Self {
        Restorer {
            map: Vec::new(),
            latest: Vec::new(),
            finder: None,
            token_like: None,
            longest: 0,
        }
    }

    /// True when there is nothing to restore.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// `text` with every surrogate put back. `before` is the character just before `text` (in a
    /// stream, the end of what was already sent), for the whole-word check.
    pub fn restore(&self, text: &str, before: Option<char>) -> String {
        let Some(finder) = &self.finder else {
            return text.to_owned();
        };
        let mut out = String::with_capacity(text.len());
        let (mut copied, mut at) = (0, 0);
        while let Some(m) = finder.find_at(text, at) {
            let prev = text[..m.start()].chars().next_back().or(if m.start() == 0 {
                before
            } else {
                None
            });
            let next = text[m.end()..].chars().next();
            let whole = !prev.is_some_and(char::is_alphanumeric)
                && !next.is_some_and(char::is_alphanumeric);
            let entry = self
                .map
                .iter()
                .find(|(s, _, _)| squashed(s) == squashed(m.as_str()));
            match entry {
                Some((surrogate, original, _)) if whole => {
                    out.push_str(&text[copied..m.start()]);
                    out.push_str(&in_case_of(m.as_str(), surrogate, original));
                    copied = m.end();
                    at = m.end();
                }
                _ => at = m.start() + text[m.start()..].chars().next().map_or(1, char::len_utf8),
            }
        }
        out.push_str(&text[copied..]);
        out
    }

    /// Footer lines about the answer as the model wrote it (before restoring): surrogates that
    /// never came back, tokens that came back changed, tokens Bullbar never sent. Empty when all is
    /// well. Names the details themselves: the footer goes only to the user.
    pub fn report(&self, answer: &str) -> Vec<String> {
        let Some(token_like) = &self.token_like else {
            return Vec::new();
        };
        let lower = squashed(answer);
        let came_back = |surrogate: &str| has_word(&lower, &squashed(surrogate));
        // A name's first or last word counts as back when the whole name is.
        // Only details from the latest user message are expected back.
        let missing: Vec<&str> = self
            .map
            .iter()
            .zip(&self.latest)
            .filter(|(_, latest)| **latest)
            .map(|(entry, _)| entry)
            .filter(|(s, _, _)| !came_back(s))
            .filter(|(s, _, kind)| {
                !(*kind == PiiKind::Person
                    && self.map.iter().any(|(whole, _, _)| {
                        whole != s && whole.split_whitespace().any(|w| w == s) && came_back(whole)
                    }))
            })
            .map(|(_, original, _)| original.as_str())
            .collect();
        let mut changed = Vec::new();
        let mut invented = Vec::new();
        for m in token_like.find_iter(answer) {
            let text = m.as_str();
            if self
                .map
                .iter()
                .any(|(s, _, _)| s.to_lowercase() == text.to_lowercase())
            {
                continue;
            }
            let Some(caps) = token_like.captures(text) else {
                continue;
            };
            let label = caps[1].to_uppercase();
            if !is_label(&label) {
                continue;
            }
            let canonical = format!("[{label}_{}]", &caps[2]);
            match self.map.iter().find(|(s, _, _)| *s == canonical) {
                Some((_, original, _)) => changed.push(format!("{text} (for {original})")),
                None if label != "CREDENTIAL" => invented.push(text.to_owned()),
                None => {}
            }
        }
        let mut lines = Vec::new();
        if !missing.is_empty() {
            lines.push(format!("not in the answer: {}", missing.join(", ")));
        }
        if !changed.is_empty() {
            lines.push(format!(
                "came back changed, left as is: {}",
                changed.join(", ")
            ));
        }
        if !invented.is_empty() {
            lines.push(format!(
                "not one Bullbar sent, left as is: {}",
                invented.join(", ")
            ));
        }
        lines
    }

    /// A restorer for one streamed answer.
    pub fn stream(&self) -> Streaming<'_> {
        Streaming {
            restorer: self,
            pending: String::new(),
            raw: String::new(),
            last: None,
        }
    }

    /// How many bytes at the end of `text` might be the start of a surrogate or a token, and must
    /// wait for what comes next.
    fn hold(&self, text: &str) -> usize {
        let mut hold = 0;
        // The tail that is the start of (or all of) a surrogate, ignoring case.
        let starts: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
        for &i in starts.iter().rev().take(self.longest) {
            // Any space character counts as a space, as in `restore`.
            let tail: String = text[i..]
                .chars()
                .map(|c| if c.is_whitespace() { ' ' } else { c })
                .collect::<String>()
                .to_lowercase();
            if self
                .map
                .iter()
                .any(|(s, _, _)| s.to_lowercase().starts_with(&tail))
            {
                hold = text.len() - i;
            }
        }
        // An unclosed `[` near the end: a token on its way.
        if let Some(open) = text.rfind('[')
            && !text[open..].contains(']')
            && text[open..].chars().count() <= TOKEN_HOLD
        {
            hold = hold.max(text.len() - open);
        }
        hold
    }
}

/// One streamed answer being restored: text arrives in pieces; what can't yet be a surrogate goes
/// out restored, the rest waits.
pub struct Streaming<'r> {
    restorer: &'r Restorer,
    pending: String,
    raw: String,
    last: Option<char>,
}

impl Streaming<'_> {
    /// Takes the next piece of the answer and returns what can go out now, restored.
    pub fn push(&mut self, piece: &str) -> String {
        self.raw.push_str(piece);
        self.pending.push_str(piece);
        let cut = self.pending.len() - self.restorer.hold(&self.pending);
        self.release(cut)
    }

    /// The end of the answer: everything still held, restored, and the footer lines about it.
    pub fn finish(&mut self) -> (String, Vec<String>) {
        let rest = self.release(self.pending.len());
        (rest, self.restorer.report(&self.raw))
    }

    fn release(&mut self, cut: usize) -> String {
        let out: String = self.pending.drain(..cut).collect();
        let restored = self.restorer.restore(&out, self.last);
        if let Some(c) = out.chars().next_back() {
            self.last = Some(c);
        }
        restored
    }
}

/// The original in the case the surrogate came back in: all lowercase or all capitals carried
/// over; anything else as the original was written. Tokens come back as the original.
fn in_case_of(found: &str, surrogate: &str, original: &str) -> String {
    if surrogate.starts_with('[') {
        return original.to_owned();
    }
    let letters = || found.chars().filter(|c| c.is_alphabetic());
    if letters().all(char::is_lowercase) {
        original.to_lowercase()
    } else if letters().count() > 1 && letters().all(char::is_uppercase) {
        original.to_uppercase()
    } else {
        original.to_owned()
    }
}

fn has_word(lower_text: &str, lower_word: &str) -> bool {
    let mut from = 0;
    while let Some(i) = lower_text[from..].find(lower_word) {
        let start = from + i;
        let end = start + lower_word.len();
        let before = lower_text[..start].chars().next_back();
        let after = lower_text[end..].chars().next();
        if !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric) {
            return true;
        }
        from = start + lower_text[start..].chars().next().map_or(1, char::len_utf8);
    }
    false
}

/// A label Bullbar uses in tokens.
fn is_label(label: &str) -> bool {
    [
        PiiKind::Person,
        PiiKind::Place,
        PiiKind::Address,
        PiiKind::Organisation,
        PiiKind::Date,
        PiiKind::Email,
        PiiKind::Phone,
        PiiKind::Id,
        PiiKind::Relationship,
    ]
    .iter()
    .any(|k| k.label() == label)
        || label == "CREDENTIAL"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn restorer() -> Restorer {
        Restorer::new(&Replaced::from_entries(&[
            (PiiKind::Person, "Imogen Fakeworth", "Noah Silva"),
            (PiiKind::Person, "Imogen", "Noah"),
            (PiiKind::Person, "Fakeworth", "Silva"),
            (PiiKind::Place, "Tobermory", "Stonebury"),
            (PiiKind::Phone, "07700 900123", "[PHONE_1]"),
            (PiiKind::Date, "22nd of August", "26th of September"),
        ]))
        .unwrap()
    }

    #[test]
    fn every_surrogate_goes_back_in_any_case_whole_words_only() {
        let r = restorer();
        assert_eq!(
            r.restore("Dear Noah Silva, Noah's cake (noah!) for 26th of September, call [PHONE_1]. Noahs stays.", None),
            "Dear Imogen Fakeworth, Imogen's cake (imogen!) for 22nd of August, call 07700 900123. Noahs stays."
        );
        assert_eq!(r.restore("STONEBURY", None), "TOBERMORY");
        assert_eq!(r.restore("Silva", Some('x')), "Silva");
    }

    #[test]
    fn credentials_are_never_restored() {
        assert_eq!(
            restorer().restore("use [CREDENTIAL_1] here", None),
            "use [CREDENTIAL_1] here"
        );
        assert!(
            restorer()
                .report("Noah Silva, Stonebury, [PHONE_1], 26th of September, [CREDENTIAL_1]")
                .is_empty()
        );
    }

    #[test]
    fn what_doesnt_come_back_exactly_is_named_not_guessed() {
        let r = restorer();
        let answer = "Hi Noah, call PHONE 1 or [PERSON_7].";
        let report = r.report(answer);
        assert_eq!(
            report,
            [
                "not in the answer: Imogen Fakeworth, Fakeworth, Tobermory, 07700 900123, 22nd of August",
                "came back changed, left as is: PHONE 1 (for 07700 900123)",
                "not one Bullbar sent, left as is: [PERSON_7]",
            ]
        );
        assert_eq!(
            r.restore(answer, None),
            "Hi Imogen, call PHONE 1 or [PERSON_7]."
        );
    }

    /// Providers write some spaces as other space characters (a narrow no-break space, U+202F, in
    /// the chunk 3 hand check): a name with one between its words is still the name.
    #[test]
    fn any_space_between_a_surrogates_words_counts() {
        let r = restorer();
        let answer =
            "Dear Noah\u{202f}Silva, ring on 26th\u{a0}of September, [PHONE_1], Stonebury.";
        assert_eq!(
            r.restore(answer, None),
            "Dear Imogen Fakeworth, ring on 22nd of August, 07700 900123, Tobermory."
        );
        assert!(r.report(answer).is_empty(), "{:?}", r.report(answer));
        let mut stream = r.stream();
        let mut out = stream.push("Dear Noah\u{202f}");
        out.push_str(&stream.push("Silva."));
        out.push_str(&stream.finish().0);
        assert_eq!(out, "Dear Imogen Fakeworth.");
    }

    #[test]
    fn a_name_back_whole_counts_for_its_parts() {
        let report = restorer().report("Noah Silva in Stonebury, [PHONE_1], 26th of September");
        assert!(report.is_empty(), "{report:?}");
    }

    /// Streams `answer` in pieces of every size and checks it comes out restored whole.
    #[test]
    fn a_surrogate_split_across_pieces_is_restored() {
        let r = restorer();
        let answer =
            "Dear Noah Silva of Stonebury, ring [PHONE_1] on 26th of September. NoahX, noah.";
        let want = r.restore(answer, None);
        for size in 1..=answer.len() {
            let mut stream = r.stream();
            let mut out = String::new();
            let bytes = answer.as_bytes();
            let mut at = 0;
            while at < bytes.len() {
                let mut end = (at + size).min(bytes.len());
                while !answer.is_char_boundary(end) {
                    end += 1;
                }
                out.push_str(&stream.push(&answer[at..end]));
                at = end;
            }
            let (rest, report) = stream.finish();
            out.push_str(&rest);
            assert_eq!(out, want, "pieces of {size}");
            assert!(report.is_empty(), "{report:?}");
        }
    }

    #[test]
    fn nothing_is_held_that_cannot_be_a_surrogate() {
        let r = restorer();
        let mut stream = r.stream();
        assert_eq!(stream.push("Hello there. "), "Hello there. ");
        assert_eq!(stream.push("Dear No"), "Dear ");
        assert_eq!(stream.push("ah, hi"), "Imogen, hi");
    }
}
