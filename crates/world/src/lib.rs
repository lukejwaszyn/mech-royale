//! Map, collision, line of sight and navigation for the Mech Royale bot. See README.md.
//!
//! Everything is re-exported at the crate root: `use world::{World, Nav, ...}`.
mod map;
mod nav;
pub use map::*;
pub use nav::*;
