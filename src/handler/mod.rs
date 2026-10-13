//! The write handler of §21: one process serves a store over a Unix-domain socket, so that many
//! producers and command lines share one loaded store and one appender.

#[doc(hidden)]
pub mod engine;
#[doc(hidden)]
pub mod pointer;
#[doc(hidden)]
pub mod protocol;
#[doc(hidden)]
pub mod server;

pub use protocol::Response;
