//! What the installed slots can actually do, given the slots each capability depends on
//! (ADR 0002, amendment). A slot being filled is not enough: persona correction needs a trusted
//! local runner as well as the Deobfuscator, and the Obfuscator may only add personas the
//! Deobfuscator can undo (ADR 0015). A missing dependency only ever removes a capability.
//!
//! Only *contract* dependencies are here: those fixed by what a slot does. Dependencies that come
//! from how one component works (a model-based classifier needing the runner) belong in its
//! manifest, once components have them.

use std::collections::BTreeSet;
use std::fmt;

use crate::decision::Mechanism;
use crate::levels::ScrubberGrade;
use crate::slots::{Slot, Slots};

/// Something a slot does that other parts of Bullbar rely on. Named as in the glossary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Capability {
    /// Keep a topic on the provider it is pinned to.
    TopicPinning,
    /// Send under the provider's persona.
    Personas,
    /// Plausible but irrelevant material inside a real query.
    DecoyContent,
    /// Additional plausible but irrelevant queries, sent as their own requests.
    Chaff,
    /// Record what each provider has seen.
    ExposureLedger,
    /// Annotate a reply for the user's real situation, undoing the persona.
    PersonaCorrection,
    /// Remove a reply's answers to decoy content.
    DecoyStripping,
    /// Scrub what only context gives away (relationships, indirect identifiers).
    ScrubByContext,
    /// Replace credentials before sending (ADR 0009).
    StripCredentials,
    /// Check everything leaving, independently of the scrubber.
    CheckOutbound,
}

/// What a capability needs: a slot filled and healthy, or another capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    /// This slot, filled and healthy.
    Slot(Slot),
    /// This capability, with everything it needs.
    Capability(Capability),
}

/// Which slot provides a capability, and what else it needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spec {
    /// The slot whose component does the work.
    pub provided_by: Slot,
    /// What it needs besides that slot.
    pub needs: &'static [Need],
}

impl Capability {
    /// Every capability.
    pub const ALL: &'static [Capability] = &[
        Capability::TopicPinning,
        Capability::Personas,
        Capability::DecoyContent,
        Capability::Chaff,
        Capability::ExposureLedger,
        Capability::PersonaCorrection,
        Capability::DecoyStripping,
        Capability::ScrubByContext,
        Capability::StripCredentials,
        Capability::CheckOutbound,
    ];

    /// The contract dependency table.
    pub fn spec(self) -> Spec {
        use Need::{Capability as Cap, Slot as S};
        let (provided_by, needs): (Slot, &'static [Need]) = match self {
            Capability::TopicPinning => (Slot::Federation, &[S(Slot::ProviderAdapters)]),
            Capability::Personas => (Slot::Obfuscator, &[Cap(Capability::PersonaCorrection)]),
            Capability::DecoyContent => (Slot::Obfuscator, &[Cap(Capability::DecoyStripping)]),
            Capability::Chaff => (Slot::Obfuscator, &[S(Slot::ProviderAdapters)]),
            Capability::ExposureLedger => (Slot::ExposureLedger, &[]),
            Capability::PersonaCorrection => (Slot::Deobfuscator, &[S(Slot::LocalRunner)]),
            // Mechanical stripping (numbered questions) is possible, but only a component can say
            // it does it reliably; until manifests exist, assume it needs a model.
            Capability::DecoyStripping => (Slot::Deobfuscator, &[S(Slot::LocalRunner)]),
            Capability::ScrubByContext => (Slot::PiiScrubber, &[S(Slot::LocalRunner)]),
            Capability::StripCredentials => (Slot::CredentialScanner, &[]),
            Capability::CheckOutbound => (Slot::OutboundCheck, &[]),
        };
        Spec { provided_by, needs }
    }

    /// The capability a protection mechanism relies on, if it needs one beyond the required
    /// slots. Scrubbing needs the PII scrubber, but a request that needs scrubbing never leaves
    /// without one (the engine keeps it local), so it is not listed here.
    pub fn of(mechanism: Mechanism) -> Option<Capability> {
        match mechanism {
            Mechanism::TopicPinning => Some(Capability::TopicPinning),
            Mechanism::Personas => Some(Capability::Personas),
            Mechanism::DecoyContent => Some(Capability::DecoyContent),
            Mechanism::Chaff => Some(Capability::Chaff),
            Mechanism::ExposureLedger => Some(Capability::ExposureLedger),
            Mechanism::Scrub | Mechanism::Restore | Mechanism::StripCredentials => None,
        }
    }

    /// Every slot this capability relies on, directly or through other capabilities, in the order
    /// they are checked: its own slot first, then each need in turn.
    pub fn slots_needed(self) -> Vec<Slot> {
        let mut slots = Vec::new();
        self.collect_slots(&mut slots, 0);
        slots
    }

    fn collect_slots(self, slots: &mut Vec<Slot>, depth: usize) {
        // The table is acyclic (tested); the depth limit only keeps a broken table from looping.
        if depth > Capability::ALL.len() {
            return;
        }
        let spec = self.spec();
        if !slots.contains(&spec.provided_by) {
            slots.push(spec.provided_by);
        }
        for need in spec.needs {
            match *need {
                Need::Slot(slot) => {
                    if !slots.contains(&slot) {
                        slots.push(slot);
                    }
                }
                Need::Capability(cap) => cap.collect_slots(slots, depth + 1),
            }
        }
    }

    /// The name used in reports, e.g. `persona correction`.
    pub fn as_str(self) -> &'static str {
        match self {
            Capability::TopicPinning => "topic pinning",
            Capability::Personas => "personas",
            Capability::DecoyContent => "decoy content",
            Capability::Chaff => "chaff",
            Capability::ExposureLedger => "exposure ledger",
            Capability::PersonaCorrection => "persona correction",
            Capability::DecoyStripping => "decoy stripping",
            Capability::ScrubByContext => "scrubbing by context",
            Capability::StripCredentials => "strip credentials",
            Capability::CheckOutbound => "outbound check",
        }
    }
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What this set of slots can actually do: the *effective* capabilities. Worked out from the slot
/// states once; a slot changing health means new [`Slots`] and so new capabilities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    have: BTreeSet<Capability>,
    /// For each capability it lacks, the first missing slot down its chain.
    lacking: Vec<(Capability, Slot)>,
}

impl Capabilities {
    /// The effective capabilities of these slots.
    pub fn new(slots: &Slots) -> Self {
        let mut have = BTreeSet::new();
        let mut lacking = Vec::new();
        for &cap in Capability::ALL {
            let mut lacks = cap.slots_needed().into_iter().find(|&s| !slots.usable(s));
            // The one capability that needs more than a working slot: scrubbing by context needs
            // a scrubber that does it, not only one that recognises entities (ADR 0014).
            if lacks.is_none()
                && cap == Capability::ScrubByContext
                && slots.scrubber_grade() < Some(ScrubberGrade::Context)
            {
                lacks = Some(Slot::PiiScrubber);
            }
            match lacks {
                None => {
                    have.insert(cap);
                }
                Some(slot) => lacking.push((cap, slot)),
            }
        }
        Capabilities { have, lacking }
    }

    /// True when the capability works with these slots.
    pub fn has(&self, cap: Capability) -> bool {
        self.have.contains(&cap)
    }

    /// The first slot down the capability's chain that isn't filled and healthy, if any.
    pub fn missing_slot(&self, cap: Capability) -> Option<Slot> {
        self.lacking
            .iter()
            .find(|(c, _)| *c == cap)
            .map(|&(_, slot)| slot)
    }

    /// The capabilities that work, in table order.
    pub fn working(&self) -> impl Iterator<Item = Capability> + '_ {
        self.have.iter().copied()
    }

    /// The capabilities that don't, each with the first slot it lacks.
    pub fn lacking(&self) -> &[(Capability, Slot)] {
        &self.lacking
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slots::SlotState;

    fn without(slot: Slot) -> Capabilities {
        Capabilities::new(&Slots::all_healthy().with(slot, SlotState::Empty))
    }

    #[test]
    fn everything_works_when_every_slot_does() {
        let caps = Capabilities::new(&Slots::all_healthy());
        assert!(Capability::ALL.iter().all(|&c| caps.has(c)));
        assert!(caps.lacking().is_empty());
    }

    #[test]
    fn personas_need_their_undo_and_so_a_local_runner() {
        let caps = without(Slot::LocalRunner);
        assert!(!caps.has(Capability::PersonaCorrection));
        assert!(!caps.has(Capability::Personas));
        assert_eq!(
            caps.missing_slot(Capability::Personas),
            Some(Slot::LocalRunner)
        );
        assert!(!without(Slot::Deobfuscator).has(Capability::Personas));
    }

    #[test]
    fn a_missing_slot_is_named_where_the_chain_breaks_first() {
        let slots = Slots::all_healthy()
            .with(Slot::Obfuscator, SlotState::Empty)
            .with(Slot::LocalRunner, SlotState::Unhealthy);
        let caps = Capabilities::new(&slots);
        assert_eq!(
            caps.missing_slot(Capability::Personas),
            Some(Slot::Obfuscator)
        );
    }

    #[test]
    fn decoy_content_needs_its_undo() {
        for slot in [Slot::Obfuscator, Slot::Deobfuscator, Slot::LocalRunner] {
            assert!(
                !without(slot).has(Capability::DecoyContent),
                "without {slot}"
            );
        }
        assert_eq!(
            Capability::DecoyContent.slots_needed(),
            [Slot::Obfuscator, Slot::Deobfuscator, Slot::LocalRunner]
        );
    }

    #[test]
    fn chaff_needs_the_obfuscator_and_somewhere_to_send_it() {
        assert!(!without(Slot::Obfuscator).has(Capability::Chaff));
        assert!(!without(Slot::ProviderAdapters).has(Capability::Chaff));
        assert!(without(Slot::LocalRunner).has(Capability::Chaff));
    }

    #[test]
    fn decoy_stripping_needs_a_local_runner_until_components_say_otherwise() {
        assert!(!without(Slot::LocalRunner).has(Capability::DecoyStripping));
        assert!(!without(Slot::Deobfuscator).has(Capability::DecoyStripping));
    }

    #[test]
    fn topic_pinning_needs_federation_and_provider_adapters() {
        assert!(!without(Slot::Federation).has(Capability::TopicPinning));
        assert!(!without(Slot::ProviderAdapters).has(Capability::TopicPinning));
    }

    #[test]
    fn scrubbing_by_context_needs_the_scrubber_and_a_local_runner() {
        assert!(!without(Slot::PiiScrubber).has(Capability::ScrubByContext));
        assert!(!without(Slot::LocalRunner).has(Capability::ScrubByContext));
    }

    #[test]
    fn scrubbing_by_context_needs_a_context_grade_scrubber() {
        let entities = Slots::all_healthy().with_scrubber_grade(ScrubberGrade::Entities);
        let caps = Capabilities::new(&entities);
        assert!(!caps.has(Capability::ScrubByContext));
        assert_eq!(
            caps.missing_slot(Capability::ScrubByContext),
            Some(Slot::PiiScrubber)
        );
    }

    #[test]
    fn the_ledger_needs_only_its_slot() {
        assert!(!without(Slot::ExposureLedger).has(Capability::ExposureLedger));
        assert_eq!(
            Capability::ExposureLedger.slots_needed(),
            [Slot::ExposureLedger]
        );
    }

    /// Following needs from any capability never comes back to it.
    #[test]
    fn the_table_is_acyclic() {
        fn reaches(from: Capability, target: Capability, depth: usize) -> bool {
            depth <= Capability::ALL.len()
                && from.spec().needs.iter().any(|need| match *need {
                    Need::Capability(c) => c == target || reaches(c, target, depth + 1),
                    Need::Slot(_) => false,
                })
        }
        for &cap in Capability::ALL {
            assert!(!reaches(cap, cap, 0), "{cap} depends on itself");
        }
    }

    #[test]
    fn every_capability_is_listed_once() {
        let unique: BTreeSet<_> = Capability::ALL.iter().collect();
        assert_eq!(unique.len(), Capability::ALL.len());
    }
}
