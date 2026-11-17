//! The component slots Bullbar is composed of (ADR 0002), and which are filled and healthy.

use std::collections::BTreeMap;

use crate::levels::ScrubberGrade;
use std::fmt;

/// A component slot. What is fixed is the set of slots, not the code that fills them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Slot {
    /// Required. Decides routes.
    DecisionEngine,
    /// Required. Checks every request on its way out, independently.
    OutboundCheck,
    /// Required. Closes paths when something looks wrong.
    Shutters,
    /// Required. Holds and evaluates the settings.
    Policy,
    /// Required. Finds credentials (ADR 0009).
    CredentialScanner,
    /// Optional. Removes personal details and relationships, and restores them.
    PiiScrubber,
    /// Optional. Rates sensitivity and complexity.
    Classifier,
    /// Optional. Runs local models.
    LocalRunner,
    /// Optional. Talks to relays and providers.
    ProviderAdapters,
    /// Optional. Remembers what each provider has seen.
    ExposureLedger,
    /// Optional. Scores provider terms into rows.
    TermsRegister,
    /// Optional. Pins topics to providers.
    Federation,
    /// Optional. Adds obfuscation on the way out: personas, decoy content, chaff (ADR 0015).
    Obfuscator,
    /// Optional. Undoes obfuscation on the reply; no tools, no network (ADR 0015).
    Deobfuscator,
    /// Optional. Leak detection.
    Canaries,
    /// Optional. Judges answers locally, so a poor one can be escalated (ADR 0013).
    AnswerCheck,
    /// Optional. User interfaces beyond the command line.
    Interfaces,
}

impl Slot {
    /// Every slot, required ones first.
    pub const ALL: &'static [Slot] = &[
        Slot::DecisionEngine,
        Slot::OutboundCheck,
        Slot::Shutters,
        Slot::Policy,
        Slot::CredentialScanner,
        Slot::PiiScrubber,
        Slot::Classifier,
        Slot::LocalRunner,
        Slot::ProviderAdapters,
        Slot::ExposureLedger,
        Slot::TermsRegister,
        Slot::Federation,
        Slot::Obfuscator,
        Slot::Deobfuscator,
        Slot::Canaries,
        Slot::AnswerCheck,
        Slot::Interfaces,
    ];

    /// Bullbar refuses to start unless this slot is filled and healthy.
    pub fn is_required(self) -> bool {
        matches!(
            self,
            Slot::DecisionEngine
                | Slot::OutboundCheck
                | Slot::Shutters
                | Slot::Policy
                | Slot::CredentialScanner
        )
    }
}

impl fmt::Display for Slot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Slot::DecisionEngine => "decision engine",
            Slot::OutboundCheck => "outbound check",
            Slot::Shutters => "shutters",
            Slot::Policy => "policy",
            Slot::CredentialScanner => "credential scanner",
            Slot::PiiScrubber => "PII scrubber",
            Slot::Classifier => "classifier",
            Slot::LocalRunner => "local runner",
            Slot::ProviderAdapters => "provider adapters",
            Slot::ExposureLedger => "exposure ledger",
            Slot::TermsRegister => "terms register",
            Slot::Federation => "federation",
            Slot::Obfuscator => "obfuscator",
            Slot::Deobfuscator => "deobfuscator",
            Slot::Canaries => "canaries",
            Slot::AnswerCheck => "answer check",
            Slot::Interfaces => "interfaces",
        })
    }
}

impl Slot {
    /// The name used in config files, e.g. `pii-scrubber`.
    pub fn key(self) -> String {
        self.to_string().to_lowercase().replace(' ', "-")
    }
}

impl std::str::FromStr for Slot {
    type Err = crate::levels::ParseLevelError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Slot::ALL
            .iter()
            .copied()
            .find(|slot| slot.key() == s)
            .ok_or_else(|| crate::levels::ParseLevelError {
                scale: "slot",
                value: s.to_owned(),
                expected: Slot::ALL
                    .iter()
                    .map(|slot| slot.key())
                    .collect::<Vec<_>>()
                    .join(", "),
            })
    }
}

/// A slot's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotState {
    /// Nothing installed.
    Empty,
    /// Installed and working.
    Healthy,
    /// Installed but failing its health check; treated as empty, never as filled.
    Unhealthy,
}

impl fmt::Display for SlotState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            SlotState::Empty => "empty",
            SlotState::Healthy => "healthy",
            SlotState::Unhealthy => "unhealthy",
        })
    }
}

/// The state of every slot. A slot not listed is empty.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Slots {
    states: BTreeMap<Slot, SlotState>,
    /// What the installed PII scrubber can find. Unstated counts as too weak (fail closed).
    scrubber_grade: Option<ScrubberGrade>,
}

impl Slots {
    /// Every slot filled and healthy, the scrubber at its best grade.
    pub fn all_healthy() -> Self {
        Slots {
            states: Slot::ALL.iter().map(|&s| (s, SlotState::Healthy)).collect(),
            scrubber_grade: Some(ScrubberGrade::Context),
        }
    }

    /// Only the required slots filled and healthy: the smallest composition that starts.
    pub fn required_only() -> Self {
        Slots {
            states: Slot::ALL
                .iter()
                .filter(|s| s.is_required())
                .map(|&s| (s, SlotState::Healthy))
                .collect(),
            scrubber_grade: None,
        }
    }

    /// This set with one slot's state changed.
    pub fn with(mut self, slot: Slot, state: SlotState) -> Self {
        self.states.insert(slot, state);
        self
    }

    /// This set with the PII scrubber's grade stated.
    pub fn with_scrubber_grade(mut self, grade: ScrubberGrade) -> Self {
        self.scrubber_grade = Some(grade);
        self
    }

    /// A slot's state.
    pub fn state(&self, slot: Slot) -> SlotState {
        self.states.get(&slot).copied().unwrap_or(SlotState::Empty)
    }

    /// The PII scrubber's grade, if stated.
    pub fn scrubber_grade(&self) -> Option<ScrubberGrade> {
        self.scrubber_grade
    }

    /// True only when the slot is filled and healthy. The PII scrubber also needs at least
    /// entity recognition: a pattern-only or ungraded scrubber counts as empty (ADR 0014).
    pub fn usable(&self, slot: Slot) -> bool {
        self.state(slot) == SlotState::Healthy
            && (slot != Slot::PiiScrubber || self.scrubber_grade >= Some(ScrubberGrade::Entities))
    }

    /// Required slots that are not filled and healthy, with their states.
    pub fn missing_required(&self) -> Vec<(Slot, SlotState)> {
        Slot::ALL
            .iter()
            .filter(|s| s.is_required() && !self.usable(**s))
            .map(|&s| (s, self.state(s)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn five_slots_are_required() {
        let required: Vec<Slot> = Slot::ALL
            .iter()
            .copied()
            .filter(|s| s.is_required())
            .collect();
        assert_eq!(
            required,
            [
                Slot::DecisionEngine,
                Slot::OutboundCheck,
                Slot::Shutters,
                Slot::Policy,
                Slot::CredentialScanner
            ]
        );
    }

    #[test]
    fn unlisted_slots_are_empty_and_unhealthy_ones_are_not_usable() {
        let slots = Slots::default().with(Slot::Classifier, SlotState::Unhealthy);
        assert_eq!(slots.state(Slot::LocalRunner), SlotState::Empty);
        assert!(!slots.usable(Slot::Classifier));
    }

    #[test]
    fn a_scrubber_needs_entity_recognition_to_count() {
        let installed = Slots::required_only().with(Slot::PiiScrubber, SlotState::Healthy);
        assert!(!installed.clone().usable(Slot::PiiScrubber), "ungraded");
        let patterns = installed
            .clone()
            .with_scrubber_grade(ScrubberGrade::Patterns);
        assert!(!patterns.usable(Slot::PiiScrubber));
        for grade in [ScrubberGrade::Entities, ScrubberGrade::Context] {
            assert!(
                installed
                    .clone()
                    .with_scrubber_grade(grade)
                    .usable(Slot::PiiScrubber)
            );
        }
        let unhealthy = Slots::all_healthy().with(Slot::PiiScrubber, SlotState::Unhealthy);
        assert!(!unhealthy.usable(Slot::PiiScrubber));
    }

    #[test]
    fn missing_required_lists_empty_and_unhealthy_ones() {
        let slots = Slots::required_only()
            .with(Slot::Shutters, SlotState::Unhealthy)
            .with(Slot::Policy, SlotState::Empty);
        assert_eq!(
            slots.missing_required(),
            [
                (Slot::Shutters, SlotState::Unhealthy),
                (Slot::Policy, SlotState::Empty)
            ]
        );
        assert!(Slots::required_only().missing_required().is_empty());
    }
}
