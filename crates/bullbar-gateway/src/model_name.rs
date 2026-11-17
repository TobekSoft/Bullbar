//! The model name a chat client asks for carries what Bullbar needs to know about the request,
//! since every chat client lets you pick a model and few let you add headers: `bullbar`,
//! `bullbar/personal`, `bullbar/public/hard/health`.
//!
//! After `bullbar`, parts come in any order. Each fills the first free slot it fits, tried as a
//! rating, then a complexity, then a category: `personal` is a rating first, so
//! `bullbar/public/personal` is a public request in the personal category. Left out, the rating is
//! unrated (which the engine treats as Sensitive), the complexity is the policy's default and the
//! category is general.

use bullbar_core::levels::{Category, Complexity, Sensitivity};
use bullbar_core::policy::Policy;
use bullbar_core::profile::RequestProfile;

/// The name every request asks for, before its parts.
pub const PREFIX: &str = "bullbar";

/// Why a model name isn't understood.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ModelNameError {
    /// Not `bullbar`, or `bullbar/` followed by parts.
    #[error(
        "ask for the model `bullbar`, optionally followed by /rating, /complexity and /category, \
         e.g. `bullbar/public/hard`; got `{0}`"
    )]
    NotBullbar(String),
    /// A part that is none of the three, or a fourth part.
    #[error(
        "`{part}` in `{name}` is not a rating (public, personal, sensitive, secret), a complexity \
         (easy, medium, hard) or a known category, or every slot is already filled"
    )]
    Unknown {
        /// The whole name.
        name: String,
        /// The part not understood.
        part: String,
    },
}

/// The request profile a model name asks for, under this policy. Only built-in categories and
/// those the policy defines are accepted, so a typo can't become a new category by accident.
pub fn profile(name: &str, policy: &Policy) -> Result<RequestProfile, ModelNameError> {
    let parts: Vec<&str> = match name.strip_prefix(PREFIX) {
        Some("") => Vec::new(),
        Some(rest) => match rest.strip_prefix('/') {
            Some(parts) => parts.split('/').collect(),
            None => return Err(ModelNameError::NotBullbar(name.to_owned())),
        },
        None => return Err(ModelNameError::NotBullbar(name.to_owned())),
    };
    let known = |category: &Category| {
        Category::BUILT_IN.contains(category)
            || policy.admin().categories.contains_key(category)
            || policy.user().categories.contains_key(category)
    };
    let (mut rating, mut complexity, mut category) = (None, None, None);
    for part in parts {
        let unknown = || ModelNameError::Unknown {
            name: name.to_owned(),
            part: part.to_owned(),
        };
        if rating.is_none()
            && let Ok(r) = part.parse::<Sensitivity>()
        {
            rating = Some(r);
        } else if complexity.is_none()
            && let Ok(c) = part.parse::<Complexity>()
        {
            complexity = Some(c);
        } else if category.is_none()
            && let Ok(c) = part.parse::<Category>()
            && known(&c)
        {
            category = Some(c);
        } else {
            return Err(unknown());
        }
    }
    Ok(RequestProfile {
        user_rating: rating,
        ..RequestProfile::new(
            complexity.unwrap_or(policy.admin().default_complexity),
            category.unwrap_or(Category::General),
        )
    })
}

/// The names worth listing for a client: `bullbar`, then each rating, then each rating with each
/// complexity.
pub fn listed() -> Vec<String> {
    let mut names = vec![PREFIX.to_owned()];
    for rating in Sensitivity::ALL {
        names.push(format!("{PREFIX}/{rating}"));
    }
    for rating in Sensitivity::ALL {
        for complexity in Complexity::ALL {
            names.push(format!("{PREFIX}/{rating}/{complexity}"));
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use bullbar_core::policy::AdminPolicy;

    fn policy() -> Policy {
        Policy::from_admin(AdminPolicy::default())
    }

    fn parse(name: &str) -> RequestProfile {
        profile(name, &policy()).unwrap()
    }

    #[test]
    fn plain_bullbar_is_unrated_at_the_default_complexity() {
        let p = parse("bullbar");
        assert_eq!(p.user_rating, None);
        assert_eq!(p.complexity, Complexity::Medium);
        assert_eq!(p.category, Category::General);
    }

    #[test]
    fn parts_come_in_any_order() {
        for name in ["bullbar/public/hard/health", "bullbar/health/hard/public"] {
            let p = parse(name);
            assert_eq!(p.user_rating, Some(Sensitivity::Public), "{name}");
            assert_eq!(p.complexity, Complexity::Hard, "{name}");
            assert_eq!(p.category, Category::Health, "{name}");
        }
    }

    #[test]
    fn personal_is_a_rating_first_then_a_category() {
        assert_eq!(
            parse("bullbar/personal").user_rating,
            Some(Sensitivity::Personal)
        );
        let p = parse("bullbar/public/personal");
        assert_eq!(p.user_rating, Some(Sensitivity::Public));
        assert_eq!(p.category, Category::Personal);
    }

    #[test]
    fn the_policy_sets_the_default_complexity() {
        let admin = AdminPolicy {
            default_complexity: Complexity::Easy,
            ..AdminPolicy::default()
        };
        let p = profile("bullbar/public", &Policy::from_admin(admin)).unwrap();
        assert_eq!(p.complexity, Complexity::Easy);
    }

    #[test]
    fn other_names_and_unknown_parts_are_refused() {
        for name in [
            "gpt",
            "bullbarx",
            "bullbar/",
            "bullbar/pubic",
            "bullbar/hard/easy",
        ] {
            assert!(profile(name, &policy()).is_err(), "{name}");
        }
        // A typo isn't a new category.
        assert!(matches!(
            profile("bullbar/public/helth", &policy()),
            Err(ModelNameError::Unknown { part, .. }) if part == "helth"
        ));
    }

    #[test]
    fn every_listed_name_parses() {
        for name in listed() {
            assert!(profile(&name, &policy()).is_ok(), "{name}");
        }
    }
}
