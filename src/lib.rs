//! tsvz: read and write tsvz-spec-v1 stores, append-only key-value logs in
//! TSV, CSV, NSV and PSV files (see `tsvz-spec-v1.md` in the TSVZ repository).
#![doc = include_str!("../README.md")]
//!
//! ## API
//!
//! The library's interface is what these docs show: [`Store`] to read, [`Writer`] and
//! [`Record`] to append, [`maint`] to clear and scrub, and the types they use. The modules
//! hidden from these docs (codecs, framing, replay, file names, locks, the command line) are
//! internals shared with the `tsvz` binary, the tests and the fuzz targets; they may change in
//! any release.

#[doc(hidden)]
pub mod codec;
#[doc(hidden)]
pub mod digest;
mod error;
#[doc(hidden)]
pub mod escape;
#[doc(hidden)]
pub mod frame;
#[doc(hidden)]
pub mod lock;
pub mod maint;
#[doc(hidden)]
pub mod name;
#[doc(hidden)]
pub mod parts;
#[doc(hidden)]
pub mod replay;
mod store;
pub mod write;

pub use error::{Error, WarnFn, Warning};
pub use name::{Codec, Format};
pub use replay::{Damage, DamageKind, Markers, Mismatch, Policy};
pub use store::{Options, PartInfo, Refreshed, Row, Scope, Store};
pub use write::{KeyKind, Record, Writer, classify_key};

#[cfg(feature = "cli")]
#[doc(hidden)]
pub mod cli;
