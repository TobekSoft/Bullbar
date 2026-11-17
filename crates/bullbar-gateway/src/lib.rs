//! Bullbar's gateway: its own endpoint in front (ADR 0017), the decision engine behind it, and the
//! provider adapters behind that. Everything async lives here; `bullbar-core` stays pure.

pub mod activity;
pub mod address;
pub mod control;
pub mod fields;
pub mod floor;
pub mod key;
pub mod model_name;
pub mod pending;
pub mod pins;
pub mod reply;
pub mod scrub;
pub mod server;
pub mod shutter;
pub mod stream;
pub mod token;
pub mod trace;
