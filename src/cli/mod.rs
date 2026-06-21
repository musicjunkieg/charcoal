//! CLI subcommand implementations that live in the library crate so they can
//! be unit-tested directly (the `charcoal` binary's clap layer is a thin shell
//! over these). See `src/main.rs` for command registration and argument wiring.

pub mod classify_compare;
pub mod classify_gate;
