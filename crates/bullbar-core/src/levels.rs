//! The scales the decision engine works on.
//!
//! Each ordered scale sorts from least to most: least sensitive, easiest, least capable, least
//! exposed. Comparisons such as `row > ceiling` therefore read as "more exposed than the ceiling".

use std::fmt;
use std::str::FromStr;

/// A level name that is not on the scale it was parsed for.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown {scale} `{value}`; expected one of: {expected}")]
pub struct ParseLevelError {
    /// The scale being parsed, e.g. "row".
    pub scale: &'static str,
    /// The text that did not match.
    pub value: String,
    /// The accepted names, comma-separated.
    pub expected: String,
}

/// Defines an ordered scale: the enum, `ALL` in order, and text names for config and the CLI.
macro_rules! scale {
    (
        $(#[$meta:meta])*
        $name:ident, $scale:literal {
            $( $(#[$vmeta:meta])* $variant:ident => $text:literal ),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum $name {
            $( $(#[$vmeta])* $variant ),+
        }

        impl $name {
            /// Every level, least to most.
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            /// The name used in config files and on the command line.
            pub const fn as_str(self) -> &'static str {
                match self {
                    $($name::$variant => $text),+
                }
            }

            /// `n` levels up, stopping at the top.
            pub fn up(self, n: u8) -> $name {
                (0..n).fold(self, |level, _| level.next().unwrap_or(level))
            }

            /// The next level up, or `None` at the top.
            pub fn next(self) -> Option<$name> {
                let i = Self::ALL.iter().position(|&l| l == self)?;
                Self::ALL.get(i + 1).copied()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = ParseLevelError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Self::ALL
                    .iter()
                    .copied()
                    .find(|l| l.as_str() == s)
                    .ok_or_else(|| ParseLevelError {
                        scale: $scale,
                        value: s.to_owned(),
                        expected: Self::ALL
                            .iter()
                            .map(|l| l.as_str())
                            .collect::<Vec<_>>()
                            .join(", "),
                    })
            }
        }
    };
}

scale! {
    /// What a PII scrubber can find (ADR 0014, amendment). Pattern matching alone doesn't fill the
    /// slot: Personal requests name people and places, which needs entity recognition at least.
    ScrubberGrade, "scrubber grade" {
        /// Patterns only (emails, phone numbers, IDs). Counts as no scrubber.
        Patterns => "patterns",
        /// Entity recognition: names, places, dates. The least that fills the slot.
        Entities => "entities",
        /// Entities, plus what only context gives away (relationships, indirect identifiers),
        /// using the local runner.
        Context => "context",
    }
}

scale! {
    /// How sensitive a request is. Sets the most exposed row allowed.
    Sensitivity, "sensitivity" {
        /// Nothing personal, e.g. a unit conversion.
        Public => "public",
        /// Mentions family, places or plans.
        Personal => "personal",
        /// Health, legal, finance, work code and the like.
        Sensitive => "sensitive",
        /// Marked secret: never leaves the local layer, whatever the settings.
        Secret => "secret",
    }
}

scale! {
    /// How hard a request is. Mapped to a minimum [`Column`] in config (ADR 0008).
    Complexity, "complexity" {
        /// Easy.
        Easy => "easy",
        /// Medium.
        Medium => "medium",
        /// Hard.
        Hard => "hard",
    }
}

scale! {
    /// A class of model capability: a column of the routing grid.
    Column, "column" {
        /// Small models.
        Small => "small",
        /// Mid-size models.
        Mid => "mid",
        /// The most capable models.
        Top => "top",
    }
}

scale! {
    /// Who gets to see a request: a row of the routing grid, least exposed first.
    Row, "row" {
        /// Nobody: answered on the local layer.
        Local => "local",
        /// The content, but not who sent it.
        AnonymousRelay => "anonymous-relay",
        /// A provider whose terms exclude training and keep little or nothing.
        ProviderNoTraining => "provider-no-training",
        /// A provider on standard terms.
        ProviderStandard => "provider-standard",
    }
}

/// True for names made of lowercase ASCII letters, digits and `-`, as used for custom categories
/// and account ids.
pub(crate) fn is_simple_name(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// What a request is about. Categories carry their own exposure ceilings in policy. Ordered only
/// so they can key sorted maps; the order means nothing.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Category {
    /// Health.
    Health,
    /// Legal.
    Legal,
    /// Finance.
    Finance,
    /// Work code.
    WorkCode,
    /// Personal life.
    Personal,
    /// Anything else.
    General,
    /// Defined by the org or the user, e.g. a client name. Lowercase letters, digits and `-`.
    Custom(String),
}

impl Category {
    /// The built-in categories.
    pub const BUILT_IN: &'static [Category] = &[
        Category::Health,
        Category::Legal,
        Category::Finance,
        Category::WorkCode,
        Category::Personal,
        Category::General,
    ];

    /// The name used in config files and on the command line.
    pub fn as_str(&self) -> &str {
        match self {
            Category::Health => "health",
            Category::Legal => "legal",
            Category::Finance => "finance",
            Category::WorkCode => "work-code",
            Category::Personal => "personal",
            Category::General => "general",
            Category::Custom(name) => name,
        }
    }
}

impl fmt::Display for Category {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Category {
    type Err = ParseLevelError;

    /// Built-in names map to their variants; any other valid name is a custom category.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Some(built_in) = Self::BUILT_IN.iter().find(|c| c.as_str() == s) {
            return Ok(built_in.clone());
        }
        if is_simple_name(s) {
            Ok(Category::Custom(s.to_owned()))
        } else {
            Err(ParseLevelError {
                scale: "category",
                value: s.to_owned(),
                expected: "a built-in category or a name of lowercase letters, digits and `-`"
                    .to_owned(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_order_from_least_to_most_exposed() {
        assert!(Row::Local < Row::AnonymousRelay);
        assert!(Row::AnonymousRelay < Row::ProviderNoTraining);
        assert!(Row::ProviderNoTraining < Row::ProviderStandard);
    }

    #[test]
    fn scales_order_from_least_to_most() {
        assert!(Sensitivity::Public < Sensitivity::Secret);
        assert!(Complexity::Easy < Complexity::Hard);
        assert!(Column::Small < Column::Top);
    }

    #[test]
    fn all_lists_levels_in_order() {
        assert!(Row::ALL.windows(2).all(|w| w[0] < w[1]));
        assert!(Column::ALL.windows(2).all(|w| w[0] < w[1]));
        assert!(Sensitivity::ALL.windows(2).all(|w| w[0] < w[1]));
        assert!(Complexity::ALL.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn next_steps_up_and_stops_at_the_top() {
        assert_eq!(Row::Local.next(), Some(Row::AnonymousRelay));
        assert_eq!(Row::ProviderStandard.next(), None);
        assert_eq!(Column::Mid.next(), Some(Column::Top));
        assert_eq!(Column::Top.next(), None);
    }

    #[test]
    fn up_climbs_and_stops_at_the_top() {
        assert_eq!(Row::Local.up(0), Row::Local);
        assert_eq!(Row::Local.up(2), Row::ProviderNoTraining);
        assert_eq!(Row::AnonymousRelay.up(9), Row::ProviderStandard);
    }

    #[test]
    fn names_round_trip() {
        for &r in Row::ALL {
            assert_eq!(r.as_str().parse::<Row>(), Ok(r));
        }
        for &c in Column::ALL {
            assert_eq!(c.to_string().parse::<Column>(), Ok(c));
        }
        for &s in Sensitivity::ALL {
            assert_eq!(s.to_string().parse::<Sensitivity>(), Ok(s));
        }
        for &c in Complexity::ALL {
            assert_eq!(c.to_string().parse::<Complexity>(), Ok(c));
        }
        for c in Category::BUILT_IN {
            assert_eq!(c.as_str().parse::<Category>().as_ref(), Ok(c));
        }
    }

    #[test]
    fn unknown_level_names_the_scale_and_the_choices() {
        let err = "huge".parse::<Column>().unwrap_err();
        assert_eq!(
            err.to_string(),
            "unknown column `huge`; expected one of: small, mid, top"
        );
    }

    #[test]
    fn custom_categories_accept_simple_names_only() {
        assert_eq!(
            "client-a7".parse::<Category>(),
            Ok(Category::Custom("client-a7".to_owned()))
        );
        assert!("".parse::<Category>().is_err());
        assert!("Client A".parse::<Category>().is_err());
    }
}
