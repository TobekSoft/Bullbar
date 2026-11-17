//! The outbound check: the last look at the exact body about to leave, after everything else.
//! Independent of the credential scanner by design (decision 6 of the chunk 2 plan, ADR 0009): its
//! own rules, written separately and a little looser, plus a check that nothing the scanner stripped
//! appears anywhere in what is sent, raw or decoded, and the same for every personal detail the
//! scrubber replaced (ADR 0020, point 7). It never changes a request; it only stops one.

use std::fmt;

use regex::Regex;
use serde_json::Value;

use super::Stripped;

/// What the outbound check found, by kind. Never the value itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    /// A private key block.
    PrivateKey,
    /// A token recognisable by its prefix, or a signed web token.
    Token,
    /// An authorisation header value.
    Authorisation,
    /// A password or key assignment.
    Assignment,
    /// A password in a URL.
    UrlPassword,
    /// A card number.
    Card,
    /// A credential the scanner had stripped, still present.
    Stripped,
    /// A personal detail the scrubber replaced, still present.
    Personal,
}

impl fmt::Display for Hit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Hit::PrivateKey => "a private key",
            Hit::Token => "a token",
            Hit::Authorisation => "an authorisation header",
            Hit::Assignment => "a password or key",
            Hit::UrlPassword => "a password in a URL",
            Hit::Card => "a card number",
            Hit::Stripped => "a credential that was stripped",
            Hit::Personal => "a personal detail that was replaced",
        })
    }
}

/// Where in the request a hit was: a path of field names and positions, such as
/// `messages[1].content`. Never the value: a field name outside the chat API's own is shown only as
/// "(a field)", since it may itself be the detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spot(String);

impl fmt::Display for Spot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Field names of the chat API, safe to show in a spot. Any other name is hidden.
const KNOWN_FIELDS: &[&str] = &[
    "model",
    "messages",
    "role",
    "content",
    "text",
    "type",
    "name",
    "tool_calls",
    "tool_call_id",
    "function",
    "arguments",
    "tools",
    "tool_choice",
    "description",
    "parameters",
    "user",
    "metadata",
    "stop",
    "response_format",
    "stream",
    "stream_options",
    "temperature",
    "top_p",
    "max_tokens",
    "max_completion_tokens",
    "seed",
    "n",
    "presence_penalty",
    "frequency_penalty",
    "logit_bias",
    "logprobs",
    "top_logprobs",
    "reasoning_effort",
    "image_url",
    "url",
    "refusal",
    "audio",
    "input_audio",
    "modalities",
    "prediction",
    "store",
    "service_tier",
    "parallel_tool_calls",
    "json_schema",
    "schema",
    "strict",
    "id",
];

/// The outbound check's rule set.
pub struct OutboundCheck {
    rules: Vec<(Hit, Regex)>,
}

impl OutboundCheck {
    /// Compiles the rules.
    pub fn new() -> Result<Self, regex::Error> {
        let rules = [
            (Hit::PrivateKey, r"BEGIN[A-Z0-9 ]*PRIVATE KEY"),
            (
                Hit::Token,
                r"\b(?:AKIA[0-9A-Z]{12,}|gh[a-z]_\w{20,}|github_pat_\w{20,}|xox[a-z]-[\w-]{8,}|AIza[\w-]{30,}|glpat-[\w-]{16,}|hf_[A-Za-z0-9]{20,}|[sr]k[-_][\w-]{16,})",
            ),
            (Hit::Token, r"eyJ[\w-]{4,}\.eyJ[\w-]{4,}\.[\w-]{4,}"),
            (
                Hit::Authorisation,
                r"(?i)\b(?:bearer|basic)\s+[\w.~+/-]{12,}",
            ),
            (
                Hit::Assignment,
                r#"(?i)(?:passw(?:or)?d|pwd|passphrase|secret|api[_-]?key|access[_-]?key|token)["']?\s*[:=]\s*["']?[^\s"',;\[\]]{6,}"#,
            ),
            (Hit::UrlPassword, r"://[^\s:/@]+:[^\s@/]+@"),
            (Hit::Card, r"(?:\d[ -]?){12,18}\d"),
        ];
        let rules = rules
            .into_iter()
            .map(|(hit, pattern)| Ok((hit, Regex::new(pattern)?)))
            .collect::<Result<_, regex::Error>>()?;
        Ok(OutboundCheck { rules })
    }

    /// Checks the body about to be sent: every decoded string (keys included) against the rules,
    /// and both the decoded strings and the raw text for anything stripped or replaced. The first
    /// hit stops it.
    pub fn check(
        &self,
        body: &Value,
        raw: &str,
        stripped: &Stripped,
        personal: &[&str],
    ) -> Result<(), (Hit, Spot)> {
        let judge = |s: &str| {
            self.check_text(s, stripped).or_else(|| {
                personal
                    .iter()
                    .any(|p| has_word(s, p))
                    .then_some(Hit::Personal)
            })
        };
        if let Some(found) = first_hit(body, &mut Vec::new(), &judge) {
            return Err(found);
        }
        let whole = || Spot("the request as written".to_owned());
        if stripped.values().any(|v| raw.contains(v)) {
            return Err((Hit::Stripped, whole()));
        }
        if personal.iter().any(|p| has_word(raw, p)) {
            return Err((Hit::Personal, whole()));
        }
        Ok(())
    }

    fn check_text(&self, text: &str, stripped: &Stripped) -> Option<Hit> {
        if stripped.values().any(|v| text.contains(v)) {
            return Some(Hit::Stripped);
        }
        for (hit, regex) in &self.rules {
            for m in regex.find_iter(text) {
                if *hit != Hit::Card || card(m.as_str()) {
                    return Some(*hit);
                }
            }
        }
        None
    }
}

/// The first string in `value` (field names included) that `judge` finds something in, and where.
fn first_hit(
    value: &Value,
    path: &mut Vec<String>,
    judge: &impl Fn(&str) -> Option<Hit>,
) -> Option<(Hit, Spot)> {
    let spot = |path: &[String], last: Option<&str>| {
        let mut out = String::new();
        for part in path.iter().map(String::as_str).chain(last) {
            if !out.is_empty() && !part.starts_with('[') {
                out.push('.');
            }
            out.push_str(part);
        }
        Spot(out)
    };
    match value {
        Value::String(s) => judge(s).map(|hit| (hit, spot(path, None))),
        Value::Array(items) => items.iter().enumerate().find_map(|(i, v)| {
            path.push(format!("[{i}]"));
            let found = first_hit(v, path, judge);
            path.pop();
            found
        }),
        Value::Object(map) => map.iter().find_map(|(key, v)| {
            let known = KNOWN_FIELDS.contains(&key.as_str());
            if let Some(hit) = judge(key) {
                let name = if known {
                    key.as_str()
                } else {
                    "(a field name)"
                };
                return Some((hit, spot(path, Some(name))));
            }
            path.push(if known {
                key.clone()
            } else {
                "(a field)".to_owned()
            });
            let found = first_hit(v, path, judge);
            path.pop();
            found
        }),
        Value::Null | Value::Bool(_) | Value::Number(_) => None,
    }
}

/// Whether `value` appears in `text` as a whole word (no letter or digit touching it), ignoring
/// case. Written apart from the scrubber's search on purpose, like `card` below.
fn has_word(text: &str, value: &str) -> bool {
    let value = value.trim().to_lowercase();
    if value.is_empty() {
        return false;
    }
    let text = text.to_lowercase();
    let mut from = 0;
    while let Some(i) = text[from..].find(&value) {
        let start = from + i;
        let end = start + value.len();
        let before = text[..start].chars().next_back();
        let after = text[end..].chars().next();
        if !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric) {
            return true;
        }
        from = start + text[start..].chars().next().map_or(1, char::len_utf8);
    }
    false
}

/// A card number: 13 to 19 digits passing the Luhn check. Written apart from the scanner's on
/// purpose: a mistake there must not be repeated here.
fn card(text: &str) -> bool {
    let digits: Vec<u32> = text
        .bytes()
        .filter(u8::is_ascii_digit)
        .map(|b| u32::from(b - b'0'))
        .collect();
    if digits.len() < 13 || digits.len() > 19 {
        return false;
    }
    let mut sum = 0;
    let mut double = false;
    for d in digits.iter().rev() {
        let mut d = *d;
        if double {
            d *= 2;
            if d > 9 {
                d -= 9;
            }
        }
        sum += d;
        double = !double;
    }
    sum.is_multiple_of(10)
}

#[cfg(test)]
mod tests {
    use super::super::FAKES;
    use super::*;
    use serde_json::json;

    fn check(body: &Value) -> Result<(), Hit> {
        OutboundCheck::new()
            .unwrap()
            .check(body, &body.to_string(), &Stripped::default(), &[])
            .map_err(|(hit, _)| hit)
    }

    #[test]
    fn every_kind_of_fake_is_caught_on_its_own() {
        for fake in FAKES {
            let body =
                json!({ "messages": [{ "role": "user", "content": format!("x {fake} y") }] });
            assert!(check(&body).is_err(), "{fake}");
        }
    }

    #[test]
    fn placeholders_and_ordinary_requests_pass() {
        let body = json!({
            "model": "m", "max_tokens": 500, "temperature": 0.2,
            "messages": [{ "role": "user", "content": "password = [CREDENTIAL_1], key [CREDENTIAL_2]" }],
        });
        assert_eq!(check(&body), Ok(()));
    }

    #[test]
    fn a_stripped_value_anywhere_is_caught() {
        let mut stripped = Stripped::default();
        stripped
            .0
            .push(("plain-looking-value".to_owned(), "a token"));
        let body = json!({ "messages": [{ "content": "ok" }], "user": "plain-looking-value" });
        let check = OutboundCheck::new().unwrap();
        assert_eq!(
            check
                .check(&body, &body.to_string(), &stripped, &[])
                .map_err(|(hit, _)| hit),
            Err(Hit::Stripped)
        );
    }

    #[test]
    fn a_hit_says_where_it_was_never_what() {
        let check = OutboundCheck::new().unwrap();
        let run = |body: &Value| {
            check
                .check(body, &body.to_string(), &Stripped::default(), &["Imogen"])
                .unwrap_err()
                .1
                .to_string()
        };
        let body = json!({ "messages": [{ "role": "user", "content": "hi" },
            { "role": "user", "content": [{ "type": "text", "text": "Ask Imogen" }] }] });
        assert_eq!(run(&body), "messages[1].content[0].text");
        assert_eq!(run(&json!({ "messages": [], "user": "imogen" })), "user");
        // A field name the API doesn't use may itself be the detail: never shown.
        assert_eq!(
            run(&json!({ "extra": { "Imogen": 1 } })),
            "(a field).(a field name)"
        );
        assert_eq!(run(&json!({ "Imogen": "x" })), "(a field name)");
        assert_eq!(
            run(&json!({ "metadata": { "x9": "Imogen" } })),
            "metadata.(a field)"
        );
    }

    #[test]
    fn a_replaced_detail_anywhere_is_caught_in_any_case() {
        let check = OutboundCheck::new().unwrap();
        let run = |body: &Value| {
            check
                .check(
                    body,
                    &body.to_string(),
                    &Stripped::default(),
                    &["Imogen Fakeworth", "Sam"],
                )
                .map_err(|(hit, _)| hit)
        };
        let body =
            json!({ "messages": [{ "content": "Ask Jane Hart" }], "user": "imogen fakeworth" });
        assert_eq!(run(&body), Err(Hit::Personal));
        let body = json!({ "messages": [{ "content": "Tell SAM" }] });
        assert_eq!(run(&body), Err(Hit::Personal));
        // A longer word holding the value isn't the value.
        let body = json!({ "messages": [{ "content": "the same Samuel" }] });
        assert_eq!(run(&body), Ok(()));
    }
}
