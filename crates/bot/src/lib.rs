//! Mech Royale bot. The binary (`main.rs`) is a thin wrapper around [`runner::run_stdio`];
//! the library form lets a simulator drive a [`runner::Session`] in-process.
pub mod ballistics;
pub mod brain;
pub mod config;
pub mod gunnery;
pub mod log;
pub mod mathx;
pub mod model;
pub mod mover;
pub mod proto;
pub mod runner;
pub mod tasks;

pub use brain::Bot;
pub use config::Tuning;
pub use runner::Session;
