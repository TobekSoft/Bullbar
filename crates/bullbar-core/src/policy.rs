//! The limits the engine routes within: the admin's hard limits and the user's values inside them
//! (ADR 0012).

use std::collections::BTreeMap;

use crate::levels::{Category, Column, Complexity, Row, Sensitivity};

/// The most exposed row each rating allows (ADR 0010 §3). Personal and Sensitive leave Local only
/// scrubbed, so theirs are scrubbed ceilings. Secret is not here: it is always Local, in code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowCeilings {
    /// For Public requests.
    pub public: Row,
    /// For Personal requests, once scrubbed.
    pub personal: Row,
    /// For Sensitive requests, once scrubbed.
    pub sensitive: Row,
}

impl RowCeilings {
    /// The ceiling for a rating. Secret is always Local.
    pub fn for_rating(&self, rating: Sensitivity) -> Row {
        match rating {
            Sensitivity::Public => self.public,
            Sensitivity::Personal => self.personal,
            Sensitivity::Sensitive => self.sensitive,
            Sensitivity::Secret => Row::Local,
        }
    }
}

impl Default for RowCeilings {
    /// Public may go anywhere; Personal and Sensitive, once scrubbed, as far as a provider that
    /// doesn't train on requests.
    fn default() -> Self {
        RowCeilings {
            public: Row::ProviderStandard,
            personal: Row::ProviderNoTraining,
            sensitive: Row::ProviderNoTraining,
        }
    }
}

/// The least capable column each complexity needs (ADR 0008).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColumnMinimums {
    /// For Easy work.
    pub easy: Column,
    /// For Medium work.
    pub medium: Column,
    /// For Hard work.
    pub hard: Column,
}

impl ColumnMinimums {
    /// The minimum column for a complexity.
    pub fn for_complexity(&self, complexity: Complexity) -> Column {
        match complexity {
            Complexity::Easy => self.easy,
            Complexity::Medium => self.medium,
            Complexity::Hard => self.hard,
        }
    }
}

impl Default for ColumnMinimums {
    /// Easy → Small, Medium → Mid, Hard → Top.
    fn default() -> Self {
        ColumnMinimums {
            easy: Column::Small,
            medium: Column::Mid,
            hard: Column::Top,
        }
    }
}

/// The admin's rule for one category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CategoryRule {
    /// The most exposed row requests in this category may reach.
    pub ceiling: Row,
    /// Firm: the user can't loosen it, no one-off exception applies, and ratings can't be lowered
    /// below the classifier's (ADR 0012 §4).
    pub locked: bool,
}

/// The admin's hard limits: an employer, a parent, a professional duty, or your own earlier self.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminPolicy {
    /// Most exposed row per rating.
    pub ceilings: RowCeilings,
    /// Rules for particular categories; a category not listed has no category limit.
    pub categories: BTreeMap<Category, CategoryRule>,
    /// The most capable column anyone may use.
    pub max_column: Column,
    /// Least capable column per complexity.
    pub minimum_columns: ColumnMinimums,
    /// Users may turn strip-and-send on (ADR 0009).
    pub strip_and_send_allowed: bool,
    /// Users may take one-off exception rungs (ADR 0011).
    pub exceptions_allowed: bool,
    /// Users may rate a request below the classifier, with confirmation.
    pub lower_ratings_allowed: bool,
    /// Users may let Sensitive requests leave, scrubbed, when there is no local model to keep them
    /// home (ADR 0014).
    pub send_sensitive_without_local_allowed: bool,
    /// How hard a request counts as when nothing says (no classifier, no rating in the request).
    pub default_complexity: Complexity,
    /// Users may run `bullbar trace`: log what left, for a while (ADR 0018 amendment).
    pub trace_allowed: bool,
}

impl Default for AdminPolicy {
    /// The default ceilings and column mapping, no category rules, every column, and the user free
    /// to opt in to strip-and-send, exceptions and lower ratings.
    fn default() -> Self {
        AdminPolicy {
            ceilings: RowCeilings::default(),
            categories: BTreeMap::new(),
            max_column: Column::Top,
            minimum_columns: ColumnMinimums::default(),
            strip_and_send_allowed: true,
            exceptions_allowed: true,
            lower_ratings_allowed: true,
            send_sensitive_without_local_allowed: true,
            default_complexity: Complexity::Medium,
            trace_allowed: true,
        }
    }
}

/// The user's own values, each within the admin's limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserPolicy {
    /// Most exposed row per rating.
    pub ceilings: RowCeilings,
    /// The user's own category ceilings; they add to or tighten the admin's.
    pub categories: BTreeMap<Category, Row>,
    /// The most capable column this user uses.
    pub max_column: Column,
    /// Credentials may be replaced so the rest of a request can leave.
    pub strip_and_send: bool,
    /// One-off exception rungs are offered.
    pub exceptions: bool,
    /// What happens when the answer check judges an answer poor (ADR 0013).
    pub escalation: Escalation,
    /// With no local model, Sensitive requests leave scrubbed instead of being refused (ADR 0014).
    pub send_sensitive_without_local: bool,
    /// With no local runner to write chaff, send canned chaff anyway. Off by default: canned
    /// chaff is easy to filter out and still costs money (ADR 0015). Not bounded by the admin:
    /// chaff adds nothing real to what leaves.
    pub chaff_without_local: bool,
}

/// What happens when the answer check judges an answer poor (ADR 0013 §3). The user's own verdict
/// always leaves the choice to them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Escalation {
    /// Escalate to a more capable model without asking.
    Auto,
    /// Escalate without asking while the request stays home; ask once before it first leaves.
    #[default]
    AskBeforeLeavingLocal,
    /// Ask before every escalation.
    Ask,
    /// Never escalate by itself: a poor answer waits for the user.
    Off,
}

impl UserPolicy {
    /// The admin's values, with strip-and-send off; exceptions and sending sensitive requests
    /// without a local model on where allowed.
    pub fn within(admin: &AdminPolicy) -> Self {
        UserPolicy {
            ceilings: admin.ceilings,
            categories: BTreeMap::new(),
            max_column: admin.max_column,
            strip_and_send: false,
            exceptions: admin.exceptions_allowed,
            escalation: Escalation::default(),
            send_sensitive_without_local: admin.send_sensitive_without_local_allowed,
            chaff_without_local: false,
        }
    }
}

impl std::str::FromStr for Escalation {
    type Err = crate::levels::ParseLevelError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "auto" => Ok(Escalation::Auto),
            "ask-before-leaving-local" => Ok(Escalation::AskBeforeLeavingLocal),
            "ask" => Ok(Escalation::Ask),
            "off" => Ok(Escalation::Off),
            _ => Err(crate::levels::ParseLevelError {
                scale: "escalation setting",
                value: s.to_owned(),
                expected: "auto, ask-before-leaving-local, ask, off".to_owned(),
            }),
        }
    }
}

/// A user value that goes past the admin's limit.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PolicyError {
    /// A ceiling more exposed than the admin's.
    #[error("the user's ceiling for {what} is {user}, more exposed than the admin's {admin}")]
    CeilingAboveAdmin {
        /// Which ceiling, e.g. "personal requests" or "the health category".
        what: String,
        /// The user's value.
        user: Row,
        /// The admin's limit.
        admin: Row,
    },
    /// A capability cap above the admin's.
    #[error("the user's capability cap is {user}, above the admin's {admin}")]
    ColumnAboveAdmin {
        /// The user's value.
        user: Column,
        /// The admin's cap.
        admin: Column,
    },
    /// A switch the admin doesn't allow.
    #[error("the user has {what} on, which the admin doesn't allow")]
    NotAllowed {
        /// Which switch.
        what: &'static str,
    },
}

/// Admin and user policy together, checked: every user value lies within the admin's limits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    admin: AdminPolicy,
    user: UserPolicy,
}

impl Policy {
    /// Checks the user's values against the admin's limits. A user value past a limit is an
    /// error, never clamped silently.
    pub fn new(admin: AdminPolicy, user: UserPolicy) -> Result<Self, PolicyError> {
        for rating in [
            Sensitivity::Public,
            Sensitivity::Personal,
            Sensitivity::Sensitive,
        ] {
            let (u, a) = (
                user.ceilings.for_rating(rating),
                admin.ceilings.for_rating(rating),
            );
            if u > a {
                return Err(PolicyError::CeilingAboveAdmin {
                    what: format!("{rating} requests"),
                    user: u,
                    admin: a,
                });
            }
        }
        for (category, &u) in &user.categories {
            if let Some(rule) = admin.categories.get(category)
                && u > rule.ceiling
            {
                return Err(PolicyError::CeilingAboveAdmin {
                    what: format!("the {category} category"),
                    user: u,
                    admin: rule.ceiling,
                });
            }
        }
        if user.max_column > admin.max_column {
            return Err(PolicyError::ColumnAboveAdmin {
                user: user.max_column,
                admin: admin.max_column,
            });
        }
        if user.strip_and_send && !admin.strip_and_send_allowed {
            return Err(PolicyError::NotAllowed {
                what: "strip-and-send",
            });
        }
        if user.exceptions && !admin.exceptions_allowed {
            return Err(PolicyError::NotAllowed {
                what: "one-off exceptions",
            });
        }
        if user.send_sensitive_without_local && !admin.send_sensitive_without_local_allowed {
            return Err(PolicyError::NotAllowed {
                what: "sending sensitive requests without a local model",
            });
        }
        Ok(Policy { admin, user })
    }

    /// A policy where the user takes the admin's values as they are.
    pub fn from_admin(admin: AdminPolicy) -> Self {
        let user = UserPolicy::within(&admin);
        Policy { admin, user }
    }

    /// The admin's hard limits.
    pub fn admin(&self) -> &AdminPolicy {
        &self.admin
    }

    /// The user's values.
    pub fn user(&self) -> &UserPolicy {
        &self.user
    }

    /// The user's ceiling for a category, if the user or the admin set one: the less exposed of
    /// the two.
    pub fn category_ceiling(&self, category: &Category) -> Option<Row> {
        let admin = self.admin.categories.get(category).map(|rule| rule.ceiling);
        let user = self.user.categories.get(category).copied();
        match (admin, user) {
            (Some(a), Some(u)) => Some(a.min(u)),
            (a, u) => a.or(u),
        }
    }

    /// The admin's hard limit for a rating in a category: nothing passes it, not even a one-off
    /// exception.
    pub fn admin_ceiling(&self, rating: Sensitivity, category: &Category) -> Row {
        let row = self.admin.ceilings.for_rating(rating);
        match self.admin.categories.get(category) {
            Some(rule) => row.min(rule.ceiling),
            None => row,
        }
    }

    /// True when the admin has locked this category.
    pub fn is_locked(&self, category: &Category) -> bool {
        self.admin
            .categories
            .get(category)
            .is_some_and(|rule| rule.locked)
    }
}

impl Default for Policy {
    /// The default admin policy, with the user taking its values.
    fn default() -> Self {
        Policy::from_admin(AdminPolicy::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn health(ceiling: Row, locked: bool) -> AdminPolicy {
        AdminPolicy {
            categories: BTreeMap::from([(Category::Health, CategoryRule { ceiling, locked })]),
            ..AdminPolicy::default()
        }
    }

    #[test]
    fn a_user_within_the_admins_limits_is_accepted() {
        let admin = AdminPolicy::default();
        let user = UserPolicy {
            max_column: Column::Mid,
            strip_and_send: true,
            ..UserPolicy::within(&admin)
        };
        assert!(Policy::new(admin, user).is_ok());
    }

    #[test]
    fn a_user_ceiling_above_the_admins_is_rejected() {
        let admin = AdminPolicy {
            ceilings: RowCeilings {
                personal: Row::AnonymousRelay,
                ..RowCeilings::default()
            },
            ..AdminPolicy::default()
        };
        let user = UserPolicy {
            ceilings: RowCeilings {
                personal: Row::ProviderStandard,
                ..admin.ceilings
            },
            ..UserPolicy::within(&admin)
        };
        assert_eq!(
            Policy::new(admin, user).unwrap_err().to_string(),
            "the user's ceiling for personal requests is provider-standard, more exposed than \
             the admin's anonymous-relay"
        );
    }

    #[test]
    fn a_user_category_ceiling_above_the_admins_is_rejected() {
        let admin = health(Row::Local, false);
        let user = UserPolicy {
            categories: BTreeMap::from([(Category::Health, Row::AnonymousRelay)]),
            ..UserPolicy::within(&admin)
        };
        assert!(matches!(
            Policy::new(admin, user),
            Err(PolicyError::CeilingAboveAdmin { .. })
        ));
    }

    #[test]
    fn a_locked_category_can_still_be_tightened() {
        let admin = health(Row::AnonymousRelay, true);
        let user = UserPolicy {
            categories: BTreeMap::from([(Category::Health, Row::Local)]),
            ..UserPolicy::within(&admin)
        };
        let policy = Policy::new(admin, user).unwrap();
        assert_eq!(policy.category_ceiling(&Category::Health), Some(Row::Local));
        assert!(policy.is_locked(&Category::Health));
    }

    #[test]
    fn switches_the_admin_forbids_are_rejected() {
        let admin = AdminPolicy {
            strip_and_send_allowed: false,
            ..AdminPolicy::default()
        };
        let user = UserPolicy {
            strip_and_send: true,
            ..UserPolicy::within(&admin)
        };
        assert_eq!(
            Policy::new(admin, user).unwrap_err().to_string(),
            "the user has strip-and-send on, which the admin doesn't allow"
        );
    }

    #[test]
    fn a_cap_above_the_admins_is_rejected() {
        let admin = AdminPolicy {
            max_column: Column::Mid,
            ..AdminPolicy::default()
        };
        let user = UserPolicy {
            max_column: Column::Top,
            ..UserPolicy::within(&admin)
        };
        assert!(matches!(
            Policy::new(admin, user),
            Err(PolicyError::ColumnAboveAdmin { .. })
        ));
    }

    #[test]
    fn a_users_own_category_adds_a_limit() {
        let admin = AdminPolicy::default();
        let user = UserPolicy {
            categories: BTreeMap::from([(Category::Finance, Row::Local)]),
            ..UserPolicy::within(&admin)
        };
        let policy = Policy::new(admin, user).unwrap();
        assert_eq!(
            policy.category_ceiling(&Category::Finance),
            Some(Row::Local)
        );
        assert_eq!(policy.category_ceiling(&Category::Legal), None);
    }
}
