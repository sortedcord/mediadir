//! Field schema registry and the presenter that turns items into display blocks.

pub mod format;
mod presenter;
mod registry;

pub use presenter::{PresentContext, Presenter};
pub use registry::FieldRegistry;
