//! The exposure ledger, as a stub: fixed counts of what each account has seen per category
//! (ADR 0010 §9). The real ledger (encrypted, decaying, kept in the vault) comes later.

use std::collections::BTreeMap;

use crate::levels::Category;
use crate::provider::AccountId;

/// How many requests of each category each account has seen, and the count past which an account
/// is skipped for that category.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ledger {
    seen: BTreeMap<(AccountId, Category), u32>,
    threshold: u32,
}

impl Ledger {
    /// An empty ledger that skips an account for a category once it has seen `threshold` requests.
    pub fn new(threshold: u32) -> Self {
        Ledger {
            seen: BTreeMap::new(),
            threshold,
        }
    }

    /// This ledger with a count recorded.
    pub fn with(mut self, account: AccountId, category: Category, count: u32) -> Self {
        self.seen.insert((account, category), count);
        self
    }

    /// The count, if it has reached the threshold: the account is skipped for this category.
    pub fn exceeded(&self, account: &AccountId, category: &Category) -> Option<u32> {
        self.seen
            .get(&(account.clone(), category.clone()))
            .copied()
            .filter(|&count| count >= self.threshold)
    }

    /// The threshold.
    pub fn threshold(&self) -> u32 {
        self.threshold
    }
}

impl Default for Ledger {
    /// Empty: nothing is skipped.
    fn default() -> Self {
        Ledger::new(u32::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_account_is_skipped_from_the_threshold_on() {
        let a: AccountId = "provider-a".parse().unwrap();
        let ledger = Ledger::new(10).with(a.clone(), Category::Health, 10).with(
            a.clone(),
            Category::Legal,
            9,
        );
        assert_eq!(ledger.exceeded(&a, &Category::Health), Some(10));
        assert_eq!(ledger.exceeded(&a, &Category::Legal), None);
        assert_eq!(ledger.exceeded(&a, &Category::Finance), None);
    }
}
