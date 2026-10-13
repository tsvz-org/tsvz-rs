//! tsvz: read and write tsvz-spec-v1 stores, append-only key-value logs in
//! TSV, CSV, NSV and PSV files (see `tsvz-spec-v1.md` in the TSVZ repository).
#![doc = include_str!("../README.md")]

pub mod codec;
pub mod digest;
mod error;
pub mod escape;
pub mod frame;
pub mod lock;
pub mod maint;
pub mod name;
pub mod parts;
pub mod replay;
pub mod store;
pub mod write;

pub use error::{Error, WarnFn, Warning};
pub use store::{Options, PartInfo, Refreshed, Row, Scope, Store};

#[cfg(feature = "cli")]
pub mod cli;
pub use write::{KeyKind, Record, Writer, classify_key};
