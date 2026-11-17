//! Config: TOML text in, validated domain types out (TODO §1.3).
//!
//! Core reads no files; the caller hands in each file's text. Errors name the file, and the line or
//! key. Five files make a setup:
//! - `providers.toml`: the accounts, their rows, offers, status and review dates;
//! - `policy.toml`: the admin's level (ADR 0012); missing fields take the documented defaults;
//! - `user.toml` (optional): the user's level, within the admin's; missing fields take the admin's;
//! - `slots.toml`: which slots are installed; anything not listed is empty (fail closed);
//! - `ledger.toml` (optional): the exposure ledger stub.
//!
//! Unknown keys are errors, so a typo never silently does nothing. Accounts, offers and categories
//! are TOML tables keyed by name, so a duplicate can't be written: TOML rejects duplicate keys.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde::{Deserialize, Deserializer};

use crate::engine::{Engine, EngineError};
use crate::ledger::Ledger;
use crate::levels::{Category, Column, Complexity, Row, ScrubberGrade, Sensitivity};
use crate::policy::{
    AdminPolicy, CategoryRule, ColumnMinimums, Escalation, Policy, PolicyError, RowCeilings,
    UserPolicy,
};
use crate::provider::{AccountId, AccountStatus, Address, Date, Offer, ProviderAccount};
use crate::slots::{Slot, SlotState, Slots};

/// The text of each config file.
#[derive(Debug, Clone, Copy)]
pub struct ConfigText<'a> {
    /// `providers.toml`.
    pub providers: &'a str,
    /// `policy.toml`.
    pub policy: &'a str,
    /// `user.toml`, if there is one.
    pub user: Option<&'a str>,
    /// `slots.toml`.
    pub slots: &'a str,
    /// `ledger.toml`, if there is one.
    pub ledger: Option<&'a str>,
}

/// A loaded, validated setup.
#[derive(Debug, Clone)]
pub struct Config {
    /// The admin's and the user's levels.
    pub policy: Policy,
    /// Every account, as configured.
    pub accounts: Vec<ProviderAccount>,
    /// Which slots are installed, and their health.
    pub slots: Slots,
    /// The exposure ledger stub.
    pub ledger: Ledger,
}

/// Why a setup didn't load.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// Not valid TOML, an unknown key, or a value of the wrong kind. The message gives the line.
    #[error("{file}: {message}")]
    Parse {
        /// The file.
        file: &'static str,
        /// The parser's message, with line and column.
        message: String,
    },
    /// Valid TOML, but a value the rules don't allow.
    #[error("{file}: `{key}`: {problem}")]
    Invalid {
        /// The file.
        file: &'static str,
        /// The key, e.g. `accounts.local.review_due`.
        key: String,
        /// What is wrong.
        problem: String,
    },
    /// The user's level goes past the admin's.
    #[error("user.toml: {0}")]
    User(#[from] PolicyError),
}

impl Config {
    /// Parses and validates a setup.
    pub fn load(text: ConfigText<'_>) -> Result<Config, ConfigError> {
        let providers: ProvidersFile = parse("providers.toml", text.providers)?;
        let accounts = providers.accounts()?;
        let admin = parse::<AdminFile>("policy.toml", text.policy)?.admin();
        let user = match text.user {
            Some(user) => parse::<UserFile>("user.toml", user)?.user(&admin),
            None => UserPolicy::within(&admin),
        };
        let policy = Policy::new(admin, user)?;
        let slots = parse::<SlotsFile>("slots.toml", text.slots)?.slots()?;
        let ledger = match text.ledger {
            Some(ledger) => parse::<LedgerFile>("ledger.toml", ledger)?.ledger(&accounts)?,
            None => Ledger::default(),
        };
        Ok(Config {
            policy,
            accounts,
            slots,
            ledger,
        })
    }

    /// The engine for this setup. Refuses to start while a required slot isn't installed.
    pub fn engine(&self) -> Result<Engine, EngineError> {
        Ok(Engine::new(
            self.policy.clone(),
            self.accounts.clone(),
            self.slots.clone(),
        )?
        .with_ledger(self.ledger.clone()))
    }
}

fn parse<'de, T: Deserialize<'de>>(file: &'static str, text: &'de str) -> Result<T, ConfigError> {
    toml::from_str(text).map_err(|e| ConfigError::Parse {
        file,
        message: e.to_string().trim_end().to_owned(),
    })
}

/// Names in config parse through `FromStr`, so errors list what is accepted.
macro_rules! deserialize_from_str {
    ($($ty:ty),+ $(,)?) => {$(
        impl<'de> Deserialize<'de> for $ty {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let text = String::deserialize(deserializer)?;
                text.parse().map_err(serde::de::Error::custom)
            }
        }
    )+};
}

deserialize_from_str!(
    Sensitivity,
    Complexity,
    Column,
    Row,
    Category,
    AccountId,
    AccountStatus,
    Escalation,
    Slot,
    ScrubberGrade,
);

/// A native TOML date, such as `2027-01-15`, without a time.
impl<'de> Deserialize<'de> for Date {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = toml::value::Datetime::deserialize(deserializer)?;
        match (value.date, value.time, value.offset) {
            (Some(d), None, None) => Date::new(d.year, d.month, d.day)
                .ok_or_else(|| serde::de::Error::custom("not a real date")),
            _ => Err(serde::de::Error::custom(
                "expected a date such as 2027-01-15, without a time",
            )),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProvidersFile {
    #[serde(default)]
    accounts: BTreeMap<AccountId, AccountEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AccountEntry {
    row: Row,
    offers: BTreeMap<Column, u32>,
    status: Option<AccountStatus>,
    review_due: Option<Date>,
    base_url: Option<String>,
    models: Option<BTreeMap<Column, String>>,
    key_file: Option<PathBuf>,
}

impl ProvidersFile {
    fn accounts(self) -> Result<Vec<ProviderAccount>, ConfigError> {
        let invalid = |id: &AccountId, field: &str, problem: &str| ConfigError::Invalid {
            file: "providers.toml",
            key: format!("accounts.{id}.{field}"),
            problem: problem.to_owned(),
        };
        self.accounts
            .into_iter()
            .map(|(id, entry)| {
                if entry.offers.is_empty() {
                    return Err(invalid(&id, "offers", "offers no column"));
                }
                if entry.row == Row::Local {
                    // A review that never happens would only look like a check.
                    if entry.review_due.is_some() {
                        return Err(invalid(&id, "review_due", "local runners have no terms to review"));
                    }
                    if entry.status == Some(AccountStatus::Demoted) {
                        return Err(invalid(&id, "status", "local runners have no terms to demote; use `out` to take one out of rotation"));
                    }
                }
                // Where it is reached: both keys or neither. Each column offered names its model,
                // and nothing else. Whether the address is local enough is checked where it is
                // used (the gateway), since that can need a name lookup.
                if entry.key_file.is_some() && entry.base_url.is_none() {
                    return Err(invalid(&id, "key_file", "only with base_url and models"));
                }
                if entry.key_file.as_ref().is_some_and(|f| f.as_os_str().is_empty()) {
                    return Err(invalid(&id, "key_file", "is empty"));
                }
                let address = match (entry.base_url, entry.models) {
                    (None, None) => None,
                    (Some(_), None) => return Err(invalid(&id, "models", "needed with base_url: the model for each column offered")),
                    (None, Some(_)) => return Err(invalid(&id, "base_url", "needed with models")),
                    (Some(base_url), Some(models)) => {
                        if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
                            return Err(invalid(&id, "base_url", "must start with http:// or https://"));
                        }
                        let offered: Vec<Column> = entry.offers.keys().copied().collect();
                        let named: Vec<Column> = models.keys().copied().collect();
                        if offered != named {
                            return Err(invalid(&id, "models", "must name a model for exactly the columns offered"));
                        }
                        if models.values().any(|m| m.trim().is_empty()) {
                            return Err(invalid(&id, "models", "a model name is empty"));
                        }
                        Some(Address {
                            base_url,
                            models,
                            key_file: entry.key_file,
                        })
                    }
                };
                let offers = entry
                    .offers
                    .into_iter()
                    .map(|(column, cost)| Offer { column, cost })
                    .collect();
                Ok(ProviderAccount {
                    status: entry.status.unwrap_or(AccountStatus::Active),
                    review_due: entry.review_due,
                    address,
                    ..ProviderAccount::new(id, entry.row, offers)
                })
            })
            .collect()
    }
}

/// Ceilings with any of the three left out.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct CeilingsEntry {
    public: Option<Row>,
    personal: Option<Row>,
    sensitive: Option<Row>,
}

impl CeilingsEntry {
    fn over(self, base: RowCeilings) -> RowCeilings {
        RowCeilings {
            public: self.public.unwrap_or(base.public),
            personal: self.personal.unwrap_or(base.personal),
            sensitive: self.sensitive.unwrap_or(base.sensitive),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MinimumsEntry {
    easy: Option<Column>,
    medium: Option<Column>,
    hard: Option<Column>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CategoryEntry {
    ceiling: Row,
    #[serde(default)]
    locked: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AdminFile {
    #[serde(default)]
    ceilings: CeilingsEntry,
    #[serde(default)]
    categories: BTreeMap<Category, CategoryEntry>,
    max_column: Option<Column>,
    minimum_columns: Option<MinimumsEntry>,
    strip_and_send_allowed: Option<bool>,
    exceptions_allowed: Option<bool>,
    lower_ratings_allowed: Option<bool>,
    send_sensitive_without_local_allowed: Option<bool>,
    default_complexity: Option<Complexity>,
    trace_allowed: Option<bool>,
}

impl AdminFile {
    fn admin(self) -> AdminPolicy {
        let base = AdminPolicy::default();
        let minimums = base.minimum_columns;
        AdminPolicy {
            ceilings: self.ceilings.over(base.ceilings),
            categories: self
                .categories
                .into_iter()
                .map(|(category, entry)| {
                    let rule = CategoryRule {
                        ceiling: entry.ceiling,
                        locked: entry.locked,
                    };
                    (category, rule)
                })
                .collect(),
            max_column: self.max_column.unwrap_or(base.max_column),
            minimum_columns: match self.minimum_columns {
                Some(m) => ColumnMinimums {
                    easy: m.easy.unwrap_or(minimums.easy),
                    medium: m.medium.unwrap_or(minimums.medium),
                    hard: m.hard.unwrap_or(minimums.hard),
                },
                None => minimums,
            },
            strip_and_send_allowed: self
                .strip_and_send_allowed
                .unwrap_or(base.strip_and_send_allowed),
            exceptions_allowed: self.exceptions_allowed.unwrap_or(base.exceptions_allowed),
            lower_ratings_allowed: self
                .lower_ratings_allowed
                .unwrap_or(base.lower_ratings_allowed),
            send_sensitive_without_local_allowed: self
                .send_sensitive_without_local_allowed
                .unwrap_or(base.send_sensitive_without_local_allowed),
            default_complexity: self.default_complexity.unwrap_or(base.default_complexity),
            trace_allowed: self.trace_allowed.unwrap_or(base.trace_allowed),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UserFile {
    #[serde(default)]
    ceilings: CeilingsEntry,
    #[serde(default)]
    categories: BTreeMap<Category, Row>,
    max_column: Option<Column>,
    strip_and_send: Option<bool>,
    exceptions: Option<bool>,
    escalation: Option<Escalation>,
    send_sensitive_without_local: Option<bool>,
    chaff_without_local: Option<bool>,
}

impl UserFile {
    /// The user's values over the admin's. Whether they stay within it is checked by
    /// [`Policy::new`].
    fn user(self, admin: &AdminPolicy) -> UserPolicy {
        let base = UserPolicy::within(admin);
        UserPolicy {
            ceilings: self.ceilings.over(base.ceilings),
            categories: self.categories,
            max_column: self.max_column.unwrap_or(base.max_column),
            strip_and_send: self.strip_and_send.unwrap_or(base.strip_and_send),
            exceptions: self.exceptions.unwrap_or(base.exceptions),
            escalation: self.escalation.unwrap_or(base.escalation),
            send_sensitive_without_local: self
                .send_sensitive_without_local
                .unwrap_or(base.send_sensitive_without_local),
            chaff_without_local: self.chaff_without_local.unwrap_or(base.chaff_without_local),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SlotsFile {
    installed: BTreeSet<Slot>,
    #[serde(default)]
    unhealthy: BTreeSet<Slot>,
    pii_scrubber_grade: Option<ScrubberGrade>,
}

impl SlotsFile {
    fn slots(self) -> Result<Slots, ConfigError> {
        if let Some(slot) = self.unhealthy.difference(&self.installed).next() {
            return Err(ConfigError::Invalid {
                file: "slots.toml",
                key: "unhealthy".to_owned(),
                problem: format!("{} is not installed", slot.key()),
            });
        }
        let invalid = |problem: &str| ConfigError::Invalid {
            file: "slots.toml",
            key: "pii_scrubber_grade".to_owned(),
            problem: problem.to_owned(),
        };
        // Stated explicitly: no default either way, so a weak scrubber can't pass as a strong one
        // and a strong one isn't quietly ignored (ADR 0014, amendment).
        let base = match (
            self.installed.contains(&Slot::PiiScrubber),
            self.pii_scrubber_grade,
        ) {
            (true, Some(grade)) => Slots::default().with_scrubber_grade(grade),
            (true, None) => {
                return Err(invalid(
                    "required when pii-scrubber is installed: patterns, entities or context",
                ));
            }
            (false, Some(_)) => return Err(invalid("pii-scrubber is not installed")),
            (false, None) => Slots::default(),
        };
        Ok(Slot::ALL.iter().fold(base, |slots, &slot| {
            let state = if self.unhealthy.contains(&slot) {
                SlotState::Unhealthy
            } else if self.installed.contains(&slot) {
                SlotState::Healthy
            } else {
                SlotState::Empty
            };
            slots.with(slot, state)
        }))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LedgerFile {
    threshold: u32,
    #[serde(default)]
    seen: BTreeMap<AccountId, BTreeMap<Category, u32>>,
}

impl LedgerFile {
    fn ledger(self, accounts: &[ProviderAccount]) -> Result<Ledger, ConfigError> {
        if self.threshold == 0 {
            return Err(ConfigError::Invalid {
                file: "ledger.toml",
                key: "threshold".to_owned(),
                problem: "must be at least 1, or every account is skipped".to_owned(),
            });
        }
        let mut ledger = Ledger::new(self.threshold);
        for (id, counts) in self.seen {
            if !accounts.iter().any(|a| a.id == id) {
                return Err(ConfigError::Invalid {
                    file: "ledger.toml",
                    key: format!("seen.{id}"),
                    problem: "no such account in providers.toml".to_owned(),
                });
            }
            for (category, count) in counts {
                ledger = ledger.with(id.clone(), category, count);
            }
        }
        Ok(ledger)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision::{Outcome, Question, Reason};
    use crate::profile::RequestProfile;

    macro_rules! edition {
        ($name:literal, user: $user:expr, ledger: $ledger:expr) => {
            ConfigText {
                providers: include_str!(concat!("../../../config/", $name, "/providers.toml")),
                policy: include_str!(concat!("../../../config/", $name, "/policy.toml")),
                user: $user,
                slots: include_str!(concat!("../../../config/", $name, "/slots.toml")),
                ledger: $ledger,
            }
        };
    }

    fn casual() -> ConfigText<'static> {
        edition!("casual", user: Some(include_str!("../../../config/casual/user.toml")), ledger: None)
    }

    fn paranoid() -> ConfigText<'static> {
        edition!(
            "paranoid",
            user: Some(include_str!("../../../config/paranoid/user.toml")),
            ledger: Some(include_str!("../../../config/paranoid/ledger.toml"))
        )
    }

    fn corporate() -> ConfigText<'static> {
        edition!("corporate", user: None, ledger: None)
    }

    fn gateway_only() -> ConfigText<'static> {
        edition!("gateway-only", user: None, ledger: None)
    }

    fn today() -> Date {
        "2026-11-15".parse().unwrap()
    }

    fn decide(
        text: ConfigText<'_>,
        rating: Sensitivity,
        complexity: Complexity,
        category: &str,
    ) -> Outcome {
        let engine = Config::load(text).unwrap().engine().unwrap();
        let profile = RequestProfile {
            user_rating: Some(rating),
            ..RequestProfile::new(complexity, category.parse().unwrap())
        };
        engine.decide(&profile, today()).outcome
    }

    fn row_of(outcome: &Outcome) -> Row {
        match outcome {
            Outcome::Route(route) => route.cell().row,
            other => panic!("expected a route, got {other}"),
        }
    }

    #[test]
    fn every_edition_loads_and_starts() {
        for text in [casual(), paranoid(), corporate(), gateway_only()] {
            let config = Config::load(text).unwrap();
            config.engine().unwrap();
        }
    }

    #[test]
    fn casual_sends_hard_public_work_to_the_cheapest_top_model() {
        let outcome = decide(casual(), Sensitivity::Public, Complexity::Hard, "general");
        assert_eq!(row_of(&outcome), Row::ProviderStandard);
    }

    #[test]
    fn casual_takes_the_users_settings() {
        let config = Config::load(casual()).unwrap();
        assert!(config.policy.user().strip_and_send);
        assert_eq!(config.policy.user().escalation, Escalation::Auto);
    }

    #[test]
    fn paranoid_keeps_sensitive_work_home() {
        let outcome = decide(
            paranoid(),
            Sensitivity::Sensitive,
            Complexity::Hard,
            "general",
        );
        assert_eq!(row_of(&outcome), Row::Local);
    }

    #[test]
    fn paranoids_ledger_skips_a_provider_for_legal_work() {
        // provider-a has seen 12 legal requests, past the threshold of 10.
        let engine = Config::load(paranoid()).unwrap().engine().unwrap();
        let profile = RequestProfile {
            user_rating: Some(Sensitivity::Public),
            ..RequestProfile::new(Complexity::Hard, Category::Legal)
        };
        let decision = engine.decide(&profile, today());
        assert!(
            decision
                .explanation
                .reasons()
                .contains(&Reason::LedgerSkipped {
                    account: "provider-a".parse().unwrap(),
                    category: Category::Legal,
                    seen: 12,
                })
        );
    }

    #[test]
    fn corporate_keeps_client_work_local() {
        let outcome = decide(
            corporate(),
            Sensitivity::Public,
            Complexity::Hard,
            "client-work",
        );
        assert!(matches!(
            outcome,
            Outcome::AskUser(Question::SettleLocal { .. })
        ));
    }

    #[test]
    fn only_corporate_forbids_tracing() {
        for (text, allowed) in [(casual(), true), (paranoid(), true), (corporate(), false)] {
            let config = Config::load(text).unwrap();
            assert_eq!(config.policy.admin().trace_allowed, allowed);
        }
    }

    #[test]
    fn corporate_without_a_user_file_runs_on_the_admins_values() {
        let config = Config::load(corporate()).unwrap();
        assert!(!config.policy.user().exceptions);
        assert!(!config.policy.user().strip_and_send);
    }

    #[test]
    fn gateway_only_sends_sensitive_work_out_scrubbed() {
        let outcome = decide(
            gateway_only(),
            Sensitivity::Sensitive,
            Complexity::Medium,
            "general",
        );
        let Outcome::Route(route) = &outcome else {
            panic!("expected a route, got {outcome}");
        };
        assert_eq!(route.cell().row, Row::ProviderNoTraining);
        assert!(
            route
                .mechanisms()
                .contains(&crate::decision::Mechanism::Scrub)
        );
    }

    #[test]
    fn gateway_only_refuses_secret_work() {
        let outcome = decide(
            gateway_only(),
            Sensitivity::Secret,
            Complexity::Easy,
            "general",
        );
        assert_eq!(outcome, Outcome::Refuse);
    }

    #[test]
    fn corporate_refuses_sensitive_work_when_its_local_runner_is_down() {
        let mut config = Config::load(corporate()).unwrap();
        config.slots = config.slots.with(Slot::LocalRunner, SlotState::Unhealthy);
        let engine = config.engine().unwrap();
        let profile = RequestProfile {
            user_rating: Some(Sensitivity::Sensitive),
            ..RequestProfile::new(Complexity::Easy, Category::General)
        };
        assert_eq!(engine.decide(&profile, today()).outcome, Outcome::Refuse);
    }

    /// The casual edition with one file replaced.
    fn error(edit: impl FnOnce(&mut ConfigText<'static>)) -> String {
        let mut text = casual();
        edit(&mut text);
        Config::load(text).unwrap_err().to_string()
    }

    #[test]
    fn an_installed_scrubber_must_state_its_grade() {
        let e = error(|t| {
            t.slots = "installed = [\"decision-engine\", \"outbound-check\", \"shutters\", \
                       \"policy\", \"credential-scanner\", \"pii-scrubber\"]\n"
        });
        assert_eq!(
            e,
            "slots.toml: `pii_scrubber_grade`: required when pii-scrubber is installed: \
             patterns, entities or context"
        );
    }

    #[test]
    fn a_grade_without_a_scrubber_is_rejected() {
        let e =
            error(|t| t.slots = "installed = [\"policy\"]\npii_scrubber_grade = \"entities\"\n");
        assert_eq!(
            e,
            "slots.toml: `pii_scrubber_grade`: pii-scrubber is not installed"
        );
    }

    #[test]
    fn a_pattern_only_scrubber_keeps_personal_work_local() {
        let mut text = casual();
        text.slots = "installed = [\"decision-engine\", \"outbound-check\", \"shutters\", \
                      \"policy\", \"credential-scanner\", \"pii-scrubber\", \"local-runner\", \
                      \"provider-adapters\", \"terms-register\"]\npii_scrubber_grade = \"patterns\"\n";
        let config = Config::load(text).unwrap();
        assert!(!config.slots.usable(Slot::PiiScrubber));
        let profile = RequestProfile {
            user_rating: Some(Sensitivity::Personal),
            ..RequestProfile::new(Complexity::Easy, "general".parse().unwrap())
        };
        let decision = config.engine().unwrap().decide(&profile, today());
        if let Outcome::Route(route) = &decision.outcome {
            assert!(!route.leaves_local(), "{}", decision.outcome);
        }
        assert!(
            decision
                .explanation
                .to_string()
                .contains("no working PII scrubber"),
            "{}",
            decision.explanation
        );
    }

    #[test]
    fn gateway_only_scrubs_by_entities_not_context() {
        let config = Config::load(gateway_only()).unwrap();
        assert_eq!(config.slots.scrubber_grade(), Some(ScrubberGrade::Entities));
        assert!(config.slots.usable(Slot::PiiScrubber));
    }

    #[test]
    fn a_loosening_user_file_is_rejected() {
        let e = error(|t| t.user = Some("[ceilings]\npersonal = \"provider-standard\"\n"));
        assert!(e.starts_with("user.toml: "), "{e}");
        insta::assert_snapshot!(e);
    }

    #[test]
    fn an_unknown_key_is_rejected_with_its_line() {
        let e = error(|t| t.user = Some("strip_and_send = true\npersnal = \"local\"\n"));
        insta::assert_snapshot!(e);
    }

    #[test]
    fn an_unknown_level_lists_the_accepted_names() {
        let e = error(|t| t.providers = "[accounts.x]\nrow = \"cloud\"\noffers = { small = 1 }\n");
        insta::assert_snapshot!(e);
    }

    #[test]
    fn a_column_offered_twice_cannot_be_written() {
        let e = error(|t| {
            t.providers = "[accounts.x]\nrow = \"local\"\noffers = { small = 0, small = 1 }\n"
        });
        assert!(e.starts_with("providers.toml: "), "{e}");
        assert!(e.contains("duplicate key"), "{e}");
    }

    #[test]
    fn a_local_account_with_a_review_date_is_rejected() {
        let e = error(|t| {
            t.providers = "[accounts.local]\nrow = \"local\"\noffers = { small = 0 }\nreview_due = 2027-01-01\n"
        });
        assert_eq!(
            e,
            "providers.toml: `accounts.local.review_due`: local runners have no terms to review"
        );
    }

    #[test]
    fn an_address_names_a_model_for_each_column_offered() {
        let local = "[accounts.local]\nrow = \"local\"\noffers = { small = 0, mid = 0 }\n";
        let with = |extra: &str| {
            let providers = format!("{local}{extra}");
            let mut text = casual();
            text.providers = &providers;
            Config::load(text).map_err(|e| e.to_string())
        };
        let config = with(
            "base_url = \"http://127.0.0.1:11434/v1\"\nmodels = { small = \"m-s\", mid = \"m-m\" }\n",
        )
        .unwrap();
        let address = config.accounts[0].address.as_ref().unwrap();
        assert_eq!(address.models[&Column::Mid], "m-m");
        for (extra, complaint) in [
            (
                "base_url = \"http://127.0.0.1/v1\"\n",
                "`accounts.local.models`",
            ),
            (
                "models = { small = \"a\", mid = \"b\" }\n",
                "`accounts.local.base_url`",
            ),
            (
                "base_url = \"ftp://127.0.0.1/\"\nmodels = { small = \"a\", mid = \"b\" }\n",
                "http:// or https://",
            ),
            (
                "base_url = \"http://127.0.0.1/v1\"\nmodels = { small = \"a\" }\n",
                "exactly the columns offered",
            ),
            (
                "base_url = \"http://127.0.0.1/v1\"\nmodels = { small = \"a\", mid = \"b\", top = \"c\" }\n",
                "exactly the columns offered",
            ),
            (
                "base_url = \"http://127.0.0.1/v1\"\nmodels = { small = \"a\", mid = \" \" }\n",
                "empty",
            ),
        ] {
            let e = with(extra).unwrap_err();
            assert!(e.contains(complaint), "{extra}: {e}");
        }
    }

    #[test]
    fn a_key_file_comes_with_an_address() {
        let e = error(
            |t| t.providers = "[accounts.x]\nrow = \"provider-standard\"\noffers = { small = 1 }\nkey_file = \"keys/x.key\"\n",
        );
        assert!(
            e.contains("`accounts.x.key_file`: only with base_url"),
            "{e}"
        );
        let providers = "[accounts.x]\nrow = \"provider-standard\"\noffers = { small = 1 }\nbase_url = \"https://x.example/v1\"\nmodels = { small = \"x-small\" }\nkey_file = \"keys/x.key\"\n";
        let mut text = casual();
        text.providers = providers;
        let config = Config::load(text).unwrap();
        let address = config.accounts[0].address.as_ref().unwrap();
        assert_eq!(
            address.key_file.as_deref(),
            Some(std::path::Path::new("keys/x.key"))
        );
    }

    #[test]
    fn a_review_date_needs_no_time() {
        let e = error(
            |t| t.providers = "[accounts.x]\nrow = \"provider-standard\"\noffers = { small = 1 }\nreview_due = 2027-01-01T10:00:00\n",
        );
        assert!(e.contains("without a time"), "{e}");
    }

    #[test]
    fn a_missing_required_slot_stops_the_engine() {
        let mut text = casual();
        text.slots = "installed = [\"decision-engine\", \"policy\"]\n";
        let config = Config::load(text).unwrap();
        assert!(matches!(
            config.engine(),
            Err(EngineError::RequiredSlots(_))
        ));
    }

    #[test]
    fn an_unhealthy_slot_must_be_installed() {
        let e = error(|t| t.slots = "installed = [\"policy\"]\nunhealthy = [\"classifier\"]\n");
        assert_eq!(e, "slots.toml: `unhealthy`: classifier is not installed");
    }

    #[test]
    fn the_ledger_names_only_known_accounts() {
        let e = error(|t| t.ledger = Some("threshold = 5\n[seen.provider-z]\nhealth = 9\n"));
        assert_eq!(
            e,
            "ledger.toml: `seen.provider-z`: no such account in providers.toml"
        );
    }
}
