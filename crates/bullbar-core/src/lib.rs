//! Bullbar's core: the types and the decision engine that decides where each request may go,
//! and explains why. Pure logic: no network, no models, no providers, no I/O.

pub mod capabilities;
pub mod config;
pub mod decision;
pub mod eligibility;
pub mod engine;
pub mod ledger;
pub mod levels;
pub mod lifecycle;
pub mod policy;
pub mod profile;
pub mod provider;
pub mod scenario;
pub mod slots;
