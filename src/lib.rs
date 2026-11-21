#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod classify;
pub mod cli;
pub mod dredgeignore;
pub mod exit;
pub mod layout;
pub mod model;
pub mod progress;
pub mod report;
pub mod rules;
pub mod scan;
pub mod secrets;
pub mod track;
pub mod walk;
