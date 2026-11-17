//! Surrogates: what a personal detail is replaced with before it leaves (ADR 0020, points 3 and 5).
//!
//! Realistic ones for names, places and dates, so the answer reads naturally; tokens (`[PHONE_1]`)
//! for the rest. Made deterministically from the value, the conversation and a per-install secret:
//! the same on every turn of one conversation, different between conversations, and not linkable
//! to the value without the secret. Nothing is stored.

#![allow(deprecated)] // `SipHasher`: see `Keyed`.

use std::hash::{Hasher, SipHasher};

use super::PiiKind;

/// A keyed hash: SipHash-2-4, a pseudo-random function keyed by 128 bits of the per-install secret.
/// The standard library's `SipHasher` is marked deprecated (for hash maps it was replaced by a
/// randomly keyed hasher) but is stable and won't be removed; it is the keyed function we need,
/// with no new dependency. What it protects: a provider seeing a surrogate can't work out the value
/// behind it, or link two conversations, without the secret.
#[derive(Clone, Copy)]
pub struct Keyed {
    k0: u64,
    k1: u64,
}

impl Keyed {
    /// Keys from the secret's text (the state folder's `scrub.secret`, 64 hex digits). `None` if it
    /// doesn't hold 32 hex digits to begin with.
    pub fn from_secret(secret: &str) -> Option<Keyed> {
        let k0 = u64::from_str_radix(secret.get(0..16)?, 16).ok()?;
        let k1 = u64::from_str_radix(secret.get(16..32)?, 16).ok()?;
        Some(Keyed { k0, k1 })
    }

    /// The hash of the parts, each ended so ("ab","c") and ("a","bc") differ.
    pub fn hash(&self, parts: &[&[u8]]) -> u64 {
        let mut h = SipHasher::new_with_keys(self.k0, self.k1);
        for part in parts {
            h.write(part);
            h.write_u8(0xff);
        }
        h.finish()
    }
}

impl std::fmt::Debug for Keyed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Keyed(hidden)")
    }
}

/// Candidate `attempt` for a value of `kind`: a realistic surrogate, or `None` where the kind gets
/// a token. `seed` is the conversation's.
pub fn candidate(
    keyed: &Keyed,
    seed: u64,
    kind: PiiKind,
    value: &str,
    attempt: u32,
) -> Option<String> {
    let pick = |salt: &str, len: usize| -> usize {
        let h = keyed.hash(&[
            &seed.to_le_bytes(),
            kind.label().as_bytes(),
            value.to_lowercase().as_bytes(),
            salt.as_bytes(),
            &attempt.to_le_bytes(),
        ]);
        // `len` is a list length, far below u64::MAX.
        usize::try_from(h % len as u64).unwrap_or(0)
    };
    match kind {
        PiiKind::Person => Some(person(value, &pick)),
        PiiKind::Place => Some(styled(value, TOWNS[pick("town", TOWNS.len())])),
        PiiKind::Date => shift_date(value, date_offset(keyed, seed)),
        PiiKind::Address
        | PiiKind::Organisation
        | PiiKind::Email
        | PiiKind::Phone
        | PiiKind::Id
        | PiiKind::Relationship => None,
    }
}

/// A name of the same shape: one word becomes a first name, two or more a first name, any middle
/// names and a surname. Lowercase stays lowercase.
fn person(value: &str, pick: &dyn Fn(&str, usize) -> usize) -> String {
    let words = value.split_whitespace().count().max(1);
    let mut out: Vec<&str> = vec![FIRST[pick("first", FIRST.len())]];
    for i in 1..words.saturating_sub(1) {
        out.push(FIRST[pick(&format!("middle{i}"), FIRST.len())]);
    }
    if words > 1 {
        out.push(LAST[pick("last", LAST.len())]);
    }
    styled(value, &out.join(" "))
}

/// The surrogate in the value's case: all lowercase if the value is, all capitals if it is.
fn styled(value: &str, surrogate: &str) -> String {
    let letters = || value.chars().filter(|c| c.is_alphabetic());
    if letters().all(char::is_lowercase) {
        surrogate.to_lowercase()
    } else if letters().count() > 1 && letters().all(char::is_uppercase) {
        surrogate.to_uppercase()
    } else {
        surrogate.to_owned()
    }
}

/// The conversation's date shift: whole weeks, so weekdays stay right, between 1 and 8 weeks either
/// way, never zero.
fn date_offset(keyed: &Keyed, seed: u64) -> i64 {
    let h = keyed.hash(&[&seed.to_le_bytes(), b"date offset"]);
    let weeks = i64::try_from(h % 8).unwrap_or(0) + 1;
    if h & (1 << 32) == 0 {
        weeks * 7
    } else {
        -weeks * 7
    }
}

const MONTHS: [&str; 12] = [
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

/// A date in the forms the scrubber finds, shifted by `days`, written the same way. `None` if it
/// can't be read as a date (it then gets a token).
pub fn shift_date(value: &str, days: i64) -> Option<String> {
    numeric(value, days).or_else(|| written(value, days))
}

/// 2026-12-03, 09/04/2027, 02.11.1990, 9/4/27 (day first, as in the UK).
fn numeric(value: &str, days: i64) -> Option<String> {
    let v = value.trim();
    if let Some([y, m, d]) = parts(v, '-').as_deref()
        && y.len() == 4
    {
        let (y, m, d) = shift(num(y)?, num(m)?, num(d)?, days)?;
        return Some(format!("{y:04}-{m:02}-{d:02}"));
    }
    for sep in ['/', '.'] {
        if let Some([d, m, y]) = parts(v, sep).as_deref() {
            let short = y.len() == 2;
            let year = if short { 2000 + num(y)? } else { num(y)? };
            let (ny, nm, nd) = shift(year, num(m)?, num(d)?, days)?;
            let width = d.len();
            let year = if short {
                format!("{:02}", ny % 100)
            } else {
                format!("{ny:04}")
            };
            return Some(if width == 2 {
                format!("{nd:02}{sep}{nm:02}{sep}{year}")
            } else {
                format!("{nd}{sep}{nm}{sep}{year}")
            });
        }
    }
    None
}

fn parts(v: &str, sep: char) -> Option<Vec<&str>> {
    let p: Vec<&str> = v.split(sep).collect();
    (p.len() == 3
        && p.iter()
            .all(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())))
    .then_some(p)
}

fn num(s: &str) -> Option<i64> {
    s.parse().ok()
}

/// 14 March 1987, 22nd of August, March 14, 1987, 3 Jun. A date without a year is shifted within
/// a year that isn't a leap year.
fn written(value: &str, days: i64) -> Option<String> {
    let words: Vec<&str> = value
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|w| !w.is_empty())
        .collect();
    let month_at = words.iter().position(|w| month(w).is_some())?;
    let (month_no, long) = month(words[month_at])?;
    let day_word = if month_at == 0 {
        words.get(1)?
    } else {
        words.first()?
    };
    let digits: String = day_word.chars().take_while(char::is_ascii_digit).collect();
    let day = num(&digits)?;
    let suffixed = day_word.len() > digits.len();
    let year_word = words
        .iter()
        .find(|w| w.len() == 4 && w.bytes().all(|b| b.is_ascii_digit()));
    let year = year_word.and_then(|y| num(y));
    let (ny, nm, nd) = shift(year.unwrap_or(2001), month_no, day, days)?;
    let name = MONTHS[usize::try_from(nm - 1).ok()?];
    let name = if long { name } else { &name[..3] };
    let day = if suffixed {
        format!("{nd}{}", suffix(nd))
    } else {
        nd.to_string()
    };
    let of = if words.contains(&"of") { " of" } else { "" };
    let mut out = if month_at == 0 {
        format!("{name} {day}")
    } else {
        format!("{day}{of} {name}")
    };
    if year.is_some() {
        let comma = if value.contains(',') { "," } else { "" };
        out = format!("{out}{comma} {ny}");
    }
    Some(out)
}

/// A month's number and whether it was written in full.
fn month(word: &str) -> Option<(i64, bool)> {
    let w = word.to_lowercase();
    MONTHS.iter().zip(1..).find_map(|(m, n)| {
        let m = m.to_lowercase();
        if w == m {
            Some((n, true))
        } else if w.len() >= 3 && m.starts_with(&w) && w.len() <= 4 {
            Some((n, false))
        } else {
            None
        }
    })
}

fn suffix(day: i64) -> &'static str {
    match (day % 10, day % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    }
}

/// A valid date moved by `days`, or `None` if it isn't valid.
fn shift(year: i64, month: i64, day: i64, days: i64) -> Option<(i64, i64, i64)> {
    let start = days_from_civil(year, month, day)?;
    Some(civil_from_days(start + days))
}

/// Days since 1970-01-01 (Howard Hinnant's algorithm), checking the date is real.
fn days_from_civil(y: i64, m: i64, d: i64) -> Option<i64> {
    let month_days = [
        31,
        28 + i64::from(leap(y)),
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if !(1..=12).contains(&m) || d < 1 || d > month_days[usize::try_from(m - 1).ok()?] {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// Common first names from many places, no attempt to match gender (ADR 0020, point 5).
const FIRST: &[&str] = &[
    "Alex", "Amira", "Ana", "Arjun", "Ben", "Carmen", "Chen", "Chloe", "Dana", "Daniel", "Eli",
    "Elena", "Emma", "Farah", "Felix", "Grace", "Hana", "Hugo", "Ines", "Isaac", "Jamal", "Jane",
    "Jonas", "Julia", "Kai", "Kemi", "Lars", "Laura", "Leo", "Lina", "Luca", "Maya", "Mei",
    "Milan", "Mira", "Nadia", "Nina", "Noah", "Omar", "Owen", "Paula", "Petra", "Quinn", "Rafael",
    "Rosa", "Ruth", "Sami", "Sara", "Theo", "Tom", "Uma", "Vera", "Viktor", "Wen", "Yara", "Yusuf",
    "Zoe", "Ada", "Ivan", "Lea",
];

const LAST: &[&str] = &[
    "Abbott",
    "Alvarez",
    "Bauer",
    "Bennett",
    "Byrne",
    "Castro",
    "Chandra",
    "Clarke",
    "Costa",
    "Dahl",
    "Doyle",
    "Ellis",
    "Fischer",
    "Fleming",
    "Garcia",
    "Grant",
    "Haddad",
    "Hale",
    "Hansen",
    "Hughes",
    "Ibrahim",
    "Jansen",
    "Kaur",
    "Keller",
    "Kowalski",
    "Lambert",
    "Lindqvist",
    "Lopez",
    "Mahler",
    "Marsh",
    "Moreau",
    "Nakamura",
    "Novak",
    "Okafor",
    "Olsen",
    "Park",
    "Pereira",
    "Quinn",
    "Reyes",
    "Rossi",
    "Santos",
    "Schmidt",
    "Sharma",
    "Silva",
    "Sorensen",
    "Tanaka",
    "Torres",
    "Turner",
    "Varga",
    "Vogel",
    "Walsh",
    "Weber",
    "Wong",
    "Young",
    "Zimmer",
    "Nolan",
    "Moss",
    "Price",
    "Reid",
    "Stone",
];

/// Invented town names: plausible, tied to no one.
const TOWNS: &[&str] = &[
    "Ashcombe",
    "Brackenford",
    "Caldermoor",
    "Dunhollow",
    "Elmstead",
    "Fernleigh",
    "Glenmarrow",
    "Harwick",
    "Ivybridge Vale",
    "Kestrel Bay",
    "Larchmont",
    "Millbrook",
    "Northwold",
    "Oakhurst",
    "Penmarrow",
    "Queensferry Mills",
    "Redcliffe",
    "Stonebury",
    "Thornwick",
    "Upton Farrow",
    "Westerleigh",
    "Yarrowdale",
    "Ambleford",
    "Birchcombe",
    "Coldharbour",
    "Daleside",
    "Easterhope",
    "Foxley",
    "Greywater",
    "Hollinsby",
];

#[cfg(test)]
mod tests {
    use super::*;

    const KEYED: Keyed = Keyed { k0: 1, k1: 2 };

    #[test]
    fn dates_shift_in_the_form_they_came() {
        assert_eq!(shift_date("2026-12-03", 7).as_deref(), Some("2026-12-10"));
        assert_eq!(shift_date("09/04/2027", -14).as_deref(), Some("26/03/2027"));
        assert_eq!(shift_date("02.11.1990", 56).as_deref(), Some("28.12.1990"));
        assert_eq!(
            shift_date("14 March 1987", 21).as_deref(),
            Some("4 April 1987")
        );
        assert_eq!(
            shift_date("22nd of August", 14).as_deref(),
            Some("5th of September")
        );
        assert_eq!(
            shift_date("March 14, 1987", -7).as_deref(),
            Some("March 7, 1987")
        );
        assert_eq!(shift_date("3 Jun", 28).as_deref(), Some("1 Jul"));
        assert_eq!(
            shift_date("31 December 2026", 7).as_deref(),
            Some("7 January 2027")
        );
        assert_eq!(shift_date("next Tuesday", 7), None);
        assert_eq!(shift_date("31/02/2026", 7), None);
    }

    #[test]
    fn the_date_offset_is_whole_weeks_and_never_zero() {
        for seed in 0..200 {
            let days = date_offset(&KEYED, seed);
            assert!(days != 0 && days % 7 == 0 && days.abs() <= 56, "{days}");
        }
    }

    #[test]
    fn names_keep_their_shape_and_case() {
        let name = |v: &str| candidate(&KEYED, 9, PiiKind::Person, v, 0).unwrap();
        assert_eq!(name("Imogen").split(' ').count(), 1);
        assert_eq!(name("Imogen Fakeworth").split(' ').count(), 2);
        assert_eq!(name("Mary Jane Fakeworth").split(' ').count(), 3);
        assert!(name("sam").chars().all(|c| c.is_lowercase()));
    }

    #[test]
    fn the_same_value_gets_the_same_surrogate_only_within_a_conversation() {
        let same = (0..50u64)
            .filter(|&seed| {
                candidate(&KEYED, seed, PiiKind::Person, "Imogen Fakeworth", 0)
                    == candidate(&KEYED, 0, PiiKind::Person, "Imogen Fakeworth", 0)
            })
            .count();
        // Seed 0 itself, and by chance a few of 3,600 pairs.
        assert!(same < 5, "{same}");
        // Case doesn't make a different value.
        assert_eq!(
            candidate(&KEYED, 3, PiiKind::Place, "Tobermory", 0).map(|s| s.to_lowercase()),
            candidate(&KEYED, 3, PiiKind::Place, "tobermory", 0)
        );
    }

    #[test]
    fn a_secret_without_hex_keys_is_refused() {
        assert!(Keyed::from_secret("not hex at all, not hex at all!!").is_none());
        assert!(Keyed::from_secret("0123").is_none());
        assert!(Keyed::from_secret(&"ab".repeat(32)).is_some());
    }
}
