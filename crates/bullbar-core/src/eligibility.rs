//! The proof a request may leave the local layer (ADR 0009, ADR 0010 §7).
//!
//! A cloud [`Route`](crate::decision::Route) cannot be built without a [`CloudEligible`], and the
//! only way to get one is [`CloudEligible::check`], which is private to this crate. So
//! Secret-rated requests and requests still holding credentials cannot be routed to the cloud by
//! construction, not only by the engine's logic.

use crate::levels::Sensitivity;

/// Where a request's credentials stand when the engine decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialState {
    /// The scanner found none.
    None,
    /// Found, and they stay in the request.
    Found,
    /// Found, and they will be replaced by placeholders before sending (strip-and-send enabled).
    Stripped,
}

/// Why a request may not leave the local layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalOnly {
    /// Rated Secret.
    SecretRating,
    /// Still holds credentials.
    Credentials,
}

/// Proof that a request passed the checks for leaving the local layer. Cannot be built outside
/// this crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CloudEligible {
    _private: (),
}

impl CloudEligible {
    /// Checks the request's effective sensitivity and credential state. Secret wins over
    /// credentials when both apply, as the stricter reason to report.
    pub(crate) fn check(
        sensitivity: Sensitivity,
        credentials: CredentialState,
    ) -> Result<CloudEligible, LocalOnly> {
        if sensitivity == Sensitivity::Secret {
            Err(LocalOnly::SecretRating)
        } else if credentials == CredentialState::Found {
            Err(LocalOnly::Credentials)
        } else {
            Ok(CloudEligible { _private: () })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_is_never_eligible() {
        for c in [
            CredentialState::None,
            CredentialState::Found,
            CredentialState::Stripped,
        ] {
            assert_eq!(
                CloudEligible::check(Sensitivity::Secret, c),
                Err(LocalOnly::SecretRating)
            );
        }
    }

    #[test]
    fn credentials_left_in_are_never_eligible() {
        for &s in &Sensitivity::ALL[..3] {
            assert_eq!(
                CloudEligible::check(s, CredentialState::Found),
                Err(LocalOnly::Credentials)
            );
        }
    }

    #[test]
    fn other_requests_are_eligible() {
        for &s in &Sensitivity::ALL[..3] {
            assert!(CloudEligible::check(s, CredentialState::None).is_ok());
            assert!(CloudEligible::check(s, CredentialState::Stripped).is_ok());
        }
    }
}
