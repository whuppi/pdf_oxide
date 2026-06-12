//! pdf_manipulator host layer.
//!
//! This entire module is added by pdf_manipulator for the Flutter bridge.
//! It is NOT part of upstream pdf_oxide.
//!
//! Shared (both targets):
//!   `constants`    — I/O buffer capacities
//!   `dispatch`     — all operation logic, calls engine APIs
//!   `binary_codec` — parse/encode the binary wire format
//!   `bridge_api`   — request routing + the two lane-body entry points
//!   `lane_state`   — the engine state owned by one lane (no locks)
//!
//! Native only (`#[cfg(not(wasm32))]`):
//!   `native/` — lane threads, condvar reader/writer, shared buffer
//!
//! WASM only (`#[cfg(wasm32)]`):
//!   `wasm/` — JS-callback reader/writer

pub mod binary_codec;
pub mod bridge_api;
pub mod constants;
pub mod dispatch;
pub mod lane_state;
pub mod positioned_write;
pub mod font_optimizer;
#[cfg(feature = "rendering")]
pub mod image_optimizer;
#[cfg(feature = "signatures")]
pub mod sign;

#[cfg(not(target_arch = "wasm32"))]
pub mod native;

#[cfg(target_arch = "wasm32")]
pub mod wasm;
