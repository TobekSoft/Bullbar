//! Provider accounts: the cloud and local endpoints a request can be routed to.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use crate::levels::{Column, ParseLevelError, Row, is_simple_name};

/// One column an account offers, at a relative cost (ADR 0010 §2). Local offers cost 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Offer {
    /// The capability class.
    pub column: Column,
    /// Relative cost; only the order matters.
    pub cost: u32,
}

/// A calendar date, enough to compare review dates (ADR 0010 §8). No time zone: the day the
/// gateway runs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Date {
    year: u16,
    month: u8,
    day: u8,
}

impl Date {
    /// A valid date, or `None`.
    pub fn new(year: u16, month: u8, day: u8) -> Option<Date> {
        let leap =
            (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400);
        let days = match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 if leap => 29,
            2 => 28,
            _ => return None,
        };
        (1..=days)
            .contains(&day)
            .then_some(Date { year, month, day })
    }
}

impl Date {
    /// The calendar date `days` after 1970-01-01, or `None` past what a `Date` holds.
    pub fn from_days_since_1970(days: i64) -> Option<Date> {
        // Howard Hinnant's civil-from-days algorithm.
        let z = days.checked_add(719_468)?;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = doy - (153 * mp + 2) / 5 + 1;
        let month = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = yoe + era * 400 + i64::from(month <= 2);
        Date::new(
            u16::try_from(year).ok()?,
            u8::try_from(month).ok()?,
            u8::try_from(day).ok()?,
        )
    }
}

impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

impl FromStr for Date {
    type Err = ParseLevelError;

    /// `YYYY-MM-DD`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = || ParseLevelError {
            scale: "date",
            value: s.to_owned(),
            expected: "a real date written YYYY-MM-DD".to_owned(),
        };
        let mut parts = s.split('-');
        let (Some(y), Some(m), Some(d), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(err());
        };
        if y.len() != 4 || m.len() != 2 || d.len() != 2 {
            return Err(err());
        }
        let (Ok(y), Ok(m), Ok(d)) = (y.parse(), m.parse(), d.parse()) else {
            return Err(err());
        };
        Date::new(y, m, d).ok_or_else(err)
    }
}

/// Whether an account is in rotation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountStatus {
    /// In rotation at its row.
    Active,
    /// Something changed and nobody has reviewed it yet: counts one row more exposed.
    Demoted,
    /// Out of rotation: never routed to.
    Out,
}

impl FromStr for AccountStatus {
    type Err = ParseLevelError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "active" => Ok(AccountStatus::Active),
            "demoted" => Ok(AccountStatus::Demoted),
            "out" => Ok(AccountStatus::Out),
            _ => Err(ParseLevelError {
                scale: "account status",
                value: s.to_owned(),
                expected: "active, demoted, out".to_owned(),
            }),
        }
    }
}

/// An account a request can be routed to: a local runner, a relay or a provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderAccount {
    /// Its name in config.
    pub id: AccountId,
    /// Who sees requests sent to it, as its terms were last reviewed.
    pub row: Row,
    /// The columns it offers.
    pub offers: Vec<Offer>,
    /// In rotation, demoted, or out.
    pub status: AccountStatus,
    /// When its terms are next due for review. Past this date it counts one row more exposed.
    pub review_due: Option<Date>,
    /// Where it is reached. Without one, the account is for dry runs only (ADR 0017).
    pub address: Option<Address>,
}

/// Where an account is reached, and the model it runs for each column it offers. Cells are
/// classes; the names a provider gives its models live here, in config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address {
    /// The base of its chat API, e.g. `http://127.0.0.1:11434/v1`.
    pub base_url: String,
    /// The model to ask for, per column offered.
    pub models: BTreeMap<Column, String>,
    /// The file holding the account's API key, if it needs one. Only the path is config; the key
    /// is read where it is used, and never printed, logged or kept in config.
    pub key_file: Option<PathBuf>,
}

impl ProviderAccount {
    /// An active account with no review date.
    pub fn new(id: AccountId, row: Row, offers: Vec<Offer>) -> Self {
        ProviderAccount {
            id,
            row,
            offers,
            status: AccountStatus::Active,
            review_due: None,
            address: None,
        }
    }
}

/// The name of an account in config, e.g. `provider-a` or `local`. Generic names only (ADR 0005).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AccountId(String);

impl AccountId {
    /// The id as written in config.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AccountId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for AccountId {
    type Err = ParseLevelError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if is_simple_name(s) {
            Ok(AccountId(s.to_owned()))
        } else {
            Err(ParseLevelError {
                scale: "account id",
                value: s.to_owned(),
                expected: "a name of lowercase letters, digits and `-`".to_owned(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_parse_and_order() {
        let a: Date = "2026-02-28".parse().unwrap();
        let b: Date = "2026-11-15".parse().unwrap();
        assert!(a < b);
        assert_eq!(b.to_string(), "2026-11-15");
        assert!("2024-02-29".parse::<Date>().is_ok());
    }

    #[test]
    fn days_since_1970_become_dates() {
        let date = |days| Date::from_days_since_1970(days).map(|d| d.to_string());
        assert_eq!(date(0).as_deref(), Some("1970-01-01"));
        assert_eq!(date(19_782).as_deref(), Some("2024-02-29"));
        assert_eq!(date(20_772).as_deref(), Some("2026-11-15"));
    }

    #[test]
    fn impossible_dates_are_rejected() {
        for bad in [
            "2026-02-29",
            "2026-13-01",
            "2026-04-31",
            "26-01-01",
            "2026-1-01",
            "today",
        ] {
            assert!(bad.parse::<Date>().is_err(), "{bad}");
        }
    }

    #[test]
    fn account_ids_accept_simple_names_only() {
        assert_eq!(
            "provider-a".parse::<AccountId>().unwrap().as_str(),
            "provider-a"
        );
        assert!("Provider A".parse::<AccountId>().is_err());
        assert!("".parse::<AccountId>().is_err());
    }
}
