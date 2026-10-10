//! rostnix builds Rust programs with Nix, one derivation per cargo unit.
//!
//! `resolve` runs during evaluation: it asks cargo for its plans and prints
//! them as Nix. `compile`, `run-build-script` and `test` run inside the
//! derivations those plans describe.

pub mod buildscript;
pub mod cargohome;
pub mod compile;
pub mod config;
pub mod emit;
pub mod flags;
pub mod graph;
pub mod lints;
pub mod localsrc;
pub mod lockfile;
pub mod lto;
pub mod metadata;
pub mod node;
pub mod resolve;
pub mod seed;
pub mod storepath;
pub mod testrun;
pub mod unitgraph;

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;
