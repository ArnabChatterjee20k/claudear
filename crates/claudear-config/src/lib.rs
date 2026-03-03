//! Configuration loading, validation, and user registry for claudear.

pub mod config;
pub mod env_writer;
pub mod users;

pub use config::*;
pub use env_writer::update_env_file;
pub use users::{ResolvedUser, UserRegistry};
