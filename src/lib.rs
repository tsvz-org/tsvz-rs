//! tsvz: read and write tsvz-spec-v1 stores, append-only key-value logs in
//! TSV, CSV, NSV and PSV files (see `tsvz-spec-v1.md` in the TSVZ repository).
#![doc = include_str!("../README.md")]
//!
//! ## API
//!
//! The library's interface is what these docs show: [`Store`] to read, [`Writer`] and
//! [`Record`] to append, [`maint`] to clear and scrub, [`handler::Client`] to talk to a
//! store's handler (§21), and the types they use. The modules
//! hidden from these docs (codecs, framing, replay, file names, locks, operations, the
//! handler's engine, protocol and server, the command line) are
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
pub mod handler;
#[doc(hidden)]
pub mod lock;
pub mod maint;
#[doc(hidden)]
pub mod name;
#[doc(hidden)]
pub mod ops;
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

/// The public types may grow before a 1.0: outside this crate an enum cannot be matched without
/// a wildcard arm, nor a struct built by a struct expression or destructured without `..`.
///
/// ```compile_fail
/// fn f(e: tsvz::Error) -> u8 {
///     match e {
///         tsvz::Error::Missing(_) => 0,
///         tsvz::Error::Io { .. } => 1,
///         tsvz::Error::UnsupportedCodec { .. } => 2,
///         tsvz::Error::BadPath(_) => 3,
///         tsvz::Error::Refused(_) => 4,
///         tsvz::Error::NoHandler(_) => 5,
///         tsvz::Error::Handler { .. } => 6,
///         tsvz::Error::HandlerLost(_) => 7,
///     }
/// }
/// ```
/// ```compile_fail
/// fn f(o: tsvz::maint::Outcome) -> u8 {
///     use tsvz::maint::Outcome::*;
///     match o { Written => 0, Unchanged => 1, Partial { .. } => 2 }
/// }
/// ```
/// ```compile_fail
/// fn f(p: tsvz::Policy) -> u8 {
///     match p { tsvz::Policy::Recover => 0, tsvz::Policy::Skip => 1, tsvz::Policy::Stop => 2 }
/// }
/// ```
/// ```compile_fail
/// fn f(k: tsvz::DamageKind) -> u8 {
///     match k { tsvz::DamageKind::ZeroFill => 0, tsvz::DamageKind::CorruptMember => 1 }
/// }
/// ```
/// ```compile_fail
/// fn f(r: tsvz::Refreshed) -> u8 {
///     use tsvz::Refreshed::*;
///     match r { Unchanged => 0, Followed => 1, Reloaded => 2 }
/// }
/// ```
/// ```compile_fail
/// fn f(c: tsvz::Codec) -> u8 {
///     use tsvz::Codec::*;
///     match c { Gzip => 0, Bzip2 => 1, Xz => 2, Zstd => 3 }
/// }
/// ```
/// ```compile_fail
/// let o = tsvz::Options { verify: true, ..tsvz::Options::default() };
/// ```
/// ```compile_fail
/// let r = tsvz::handler::Response { status: 0, ..Default::default() };
/// ```
/// ```compile_fail
/// fn f(w: tsvz::Warning) { let tsvz::Warning { part, offset, message } = w; }
/// ```
/// ```compile_fail
/// fn f(m: tsvz::Markers) {
///     let tsvz::Markers { version, defaults, strip, fill_empty, return_missing, on_corruption,
///                         rotate, write_ack, member_size } = m;
/// }
/// ```
/// ```compile_fail
/// fn f(m: tsvz::Mismatch) { let tsvz::Mismatch { part, line, algo, expected, computed } = m; }
/// ```
/// ```compile_fail
/// fn f(d: tsvz::Damage) { let tsvz::Damage { part, offset, kind } = d; }
/// ```
/// ```compile_fail
/// fn f(p: tsvz::PartInfo) { let tsvz::PartInfo { index, ordinal, path, codec, active } = p; }
/// ```
///
/// What works, and keeps working:
///
/// ```
/// let mut options = tsvz::Options::default();
/// options.verify = true;
/// let r = tsvz::handler::Response::status(0);
/// assert_eq!(r.status, 0);
/// ```
#[cfg(doctest)]
pub struct NonExhaustive;
