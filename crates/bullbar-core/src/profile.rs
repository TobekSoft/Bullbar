//! What the engine knows about a request: its ratings, never its content.

use crate::levels::{Category, Column, Complexity, Row, Sensitivity};
use crate::provider::AccountId;

/// The engine's view of one request. Built by the classifier, the credential scanner and the
/// user's choices; it carries ratings and flags, never the request's text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestProfile {
    /// The classifier's rating, if a classifier ran and rated it.
    pub classifier_rating: Option<Sensitivity>,
    /// The user's rating for this request, if they gave one.
    pub user_rating: Option<Sensitivity>,
    /// How hard the request is.
    pub complexity: Complexity,
    /// What the request is about.
    pub category: Category,
    /// The credential scanner found credentials in it (ADR 0009).
    pub credentials_found: bool,
    /// The most exposed row this request may reach, e.g. Local for "local only". Tightens only.
    pub row_limit: Option<Row>,
    /// Asks for at least this column, within the user's cap.
    pub wanted_column: Option<Column>,
    /// A local-only shutter is closed (the kill switch): nothing leaves the local layer.
    pub shutter_closed: bool,
    /// The cloud account this request's topic (its category) is pinned to, if any. With topic
    /// pinning working, Personal and Sensitive work in the topic goes only there, or asks
    /// (ADR 0021).
    pub pinned_to: Option<AccountId>,
}

impl RequestProfile {
    /// A profile with only complexity and category set: unrated, no credentials, no overrides.
    pub fn new(complexity: Complexity, category: Category) -> Self {
        RequestProfile {
            classifier_rating: None,
            user_rating: None,
            complexity,
            category,
            credentials_found: false,
            row_limit: None,
            wanted_column: None,
            shutter_closed: false,
            pinned_to: None,
        }
    }
}
