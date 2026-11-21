//! Rough local match simulator for the `mechfight/1` bot protocol.
//!
//! This is an APPROXIMATION written from protocol.md, not the real server. Scores from it
//! do not predict the real leaderboard. See README.md.
pub mod config;
pub mod obs;
pub mod physics;
pub mod proc;
pub mod render;
pub mod runner;
pub mod scripted;
pub mod sim;
pub mod testworld;
pub mod util;
