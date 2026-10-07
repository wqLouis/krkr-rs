//! Re-export of the platform-agnostic input DTOs.
//!
//! The [`InputEvent`]/[`MouseButton`] values are pure DTOs and are defined in
//! [`api::input`]; this module re-exports them so `platform::InputEvent` and
//! `platform::MouseButton` keep their historical paths. The backend-specific
//! translation into these values lives in the L1 backends (`linux`,
//! `android`).

pub use api::input::{InputEvent, MouseButton};
