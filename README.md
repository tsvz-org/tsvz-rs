# tsvz-rs

A fast implementation of [tsvz-spec-v1](https://tsvz.org) in Rust: the `tsvz` command-line
tool, and the `tsvz` library crate it is built on. TSVZ stores are append-only key-value logs
in plain TSV, CSV, NSV or PSV text; see the specification and the Python reference
implementation, [TSVZ](https://github.com/yufei-pan/TSVZ).

## Install

```bash
cargo install tsvz
```

or download a binary for Linux (x86_64, aarch64; static) or macOS (arm64, x86_64) from the
GitHub releases. TSVZ.py also installs a `tsvz` command; both speak the same specification, so
whichever comes first on your `PATH` works the same on spec stores.

## Use

```bash
tsvz set people.tsvz alice Alice 30
tsvz get people.tsvz alice
tsvz read people.tsvz | tsvz set copy.tsvz -
tsvz verify people.tsvz
tsvz --help
```

Every operation of §20 is supported. Stores may be compressed (gzip, bzip2, xz, zstd) and split
into parts; all four digests (crc32, crc32c, sha256, blake3) are verified.

## Handler

`tsvz serve STORE` runs a write handler (§21) in the foreground: one process keeps the store
loaded and appends every write, so that many producers share one appender. While it runs, every
`tsvz` command on that store goes through it and prints what it would print on the files;
`--x-direct` works on the files instead. `tsvz stop STORE` (or Ctrl-C, SIGTERM, SIGHUP) makes
every acknowledged write durable and stops it.

```bash
tsvz serve people.tsvz &              # --x-idle-timeout 600 stops it after 10 idle minutes
tsvz set people.tsvz bob Bob 41       # through the handler
tsvz -v get people.tsvz bob           # -v names the handler it used
tsvz stop people.tsvz
```

The handler and TSVZ.py's (`tsvz serve`, `TSVZClient`) speak the same protocol: either tool's
commands go through either tool's handler. It serves strict stores (`.tsvz .csvz .nsvz .psvz`).
Its socket is private to its user; `--x-group G` lets a group connect, `--x-mode M` sets the
socket's mode. Rust programs talk to a handler with `tsvz::handler::Client`.

## Library

```rust,no_run
use tsvz::{Options, Record, Store, Writer};

let mut w = Writer::open("people.tsvz")?;
w.append(&[Record::from_args("alice", &["Alice".into(), "30".into()])])?;
let store = Store::open("people.tsvz", Options::default())?;
for (key, row) in store.iter() {
    println!("{key}: {:?}", row.to_vec());
}
# Ok::<(), tsvz::Error>(())
```

Appends take the same file locks as TSVZ.py. Where open-file-description locks are not
available (macOS, or an old Linux kernel), the lock is a classic POSIX record lock, which the
process loses when it closes any other descriptor of the same file; a program that keeps a store
open should not open its files by other means while it writes.

Build without the command line and C libraries with
`default-features = false, features = ["bzip2"]` (gzip and all digests are always in).

## Conformance and compatibility

tsvz-rs passes the reader, writer, cli and handler classes of the TSVZ conformance suite with
every codec and digest claimed, and is checked against TSVZ.py on random stores, with and
without its handler. Rust and Python
writers can share a store: they take the same file locks. Where the specification leaves a
choice open, tsvz-rs does what TSVZ.py does; [docs/silent-choices.md](docs/silent-choices.md)
lists those choices. Benchmarks against TSVZ.py are in [bench/RESULTS.md](bench/RESULTS.md).

## License

GPL-3.0-or-later.
