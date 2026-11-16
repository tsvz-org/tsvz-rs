# tsvz-rs

A fast implementation of [tsvz-spec-v1](https://tsvz.org) in Rust: the `tsvz` command-line
tool, and the `tsvz` library crate it is built on. TSVZ stores are append-only key-value logs
in plain TSV, CSV, NSV or PSV text; see the specification and the Python reference
implementation, [TSVZ](https://github.com/tsvz-org/TSVZ).

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
into parts; all four digests (crc32, crc32c, sha256, blake3) are verified. `scrub` compacts a
one-part store in place. In a multi-part store it writes the parts before the active one as a
new snapshot part, linked in whole (with no ordinal free, it compacts them in place), then
applies the store's `#_rotate_#`; writers keep appending throughout. A read begins at the last
snapshot part (§19.10), but for a bzip2 part, which is read like any other (the result is the
same); `verify` replays every part. `scrub` and `clear` refuse parts that
declare a spec version newer than tsvz implements (§6.3).

Loose files (`.tsv .csv .nsv .psv`, §20.2.4) are read as TSVZ.py's command line reads them,
except where [docs/limitations.md](docs/limitations.md) says otherwise: tsvz-rs ignores an
unterminated last line, reads `#_defaults_#` lines as comments (it fills no empty cell from
them), strips and decodes no key or cell, and keeps the empty cells at the end of a row longer
than the file's column count. A rewrite changes no line it keeps, but for a byte-order mark an
unterminated first line loses: `scrub` keeps the lines a read depends on (a key's last line may
take the place of its first, where nothing TSVZ.py reads changes), and `clear` the
`#_defaults_#` lines in force ([docs/silent-choices.md](docs/silent-choices.md)). One more
limit an operator can meet: a `delete` of a loose file that runs at the very moment of a
`clear` of it can leave the key as a row.

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

A read begins at the store's last snapshot part (§19.10.3, a bzip2 part excepted); an `Options` whose `full_history`
is set (`let mut o = Options::default(); o.full_history = true;`) replays every part, as
`verify` always does.

Appends take the same file locks as TSVZ.py. Where open-file-description locks are not
available (macOS, or an old Linux kernel), the lock is a classic POSIX record lock, which the
process loses when it closes any other descriptor of the same file. The library opens a store's
files as `tsvz::lock::Handle`s, which it closes only once it holds no such lock on them; a
program that opens a store's files itself while it writes should do the same.

Build without the command line and C libraries with
`default-features = false, features = ["bzip2"]` (gzip and all digests are always in).

## Conformance and compatibility

tsvz-rs passes the reader, writer, cli and handler classes of the TSVZ conformance suite with
every codec, every digest, multi-part scrub and snapshot skipping claimed, and is checked
against TSVZ.py on random stores, with and without its handler. Rust and Python writers can
share a store: they take the same file locks. Where the specification leaves a choice open,
tsvz-rs does what TSVZ.py does; [docs/silent-choices.md](docs/silent-choices.md) lists those
choices. Benchmarks against TSVZ.py are in [bench/RESULTS.md](bench/RESULTS.md).

## Upgrading to 0.1.4 from 0.1.3

In strict stores (`.tsvz .csvz .nsvz .psvz`) a read begins at the last snapshot part (§19.10.3),
a multi-part `scrub` writes snapshot parts that begin with `#_snapshot_#` (and makes one marked
copy of a part a 0.1.3 scrub wrote), and a writer now refuses `#_snapshot_#` (§19.10.2; 0.1.3
accepted it and wrote it as a record). A store with no snapshot part reads as before. Upgrade every host that scrubs the
same multi-part store: a 0.1.3 scrub after a 0.1.4 one writes a new snapshot part each time
(0.1.3 does not expect the marker line), and the next 0.1.4 scrub adds a marked copy of it.

In loose files:

- A key alone is a row when the file's first keyed line has one column; `delete`, `pop` and
  `popitem` there rewrite the file without the key's lines. With more columns they append a key
  alone for each key asked, as before, and also without the whitespace at its end when it has
  some (TSVZ.py strips it when it reads a key; tsvz-rs does not).
- A row whose cells are all blank is a deletion, as in TSVZ.py, but in a file of one column,
  where it is a row: in any other file `tsvz set f.tsv k ''` deletes `k`, and a file holding
  `k<TAB>` reads without `k`.
- Line 1's byte-order mark is part of its key, as TSVZ.py reads it (`keys` lists it, where 0.1.3
  dropped it); the NULs at the start of a line are dropped; a key that only begins with
  `#_defaults_#` is a key.
- `scrub` keeps each line it keeps as the file has it, `#_defaults_#` lines included, and
  `clear` keeps the `#_defaults_#` lines in force, so that rows written later read with the
  defaults they had (a file of one column keeps none: rows written there later read without
  them).

The library gains `Options::full_history`; nothing else in its documented API changes (two
`#[doc(hidden)]` items do: `codec::member` and `codec::Member` are gone, and `replay::Replay`
has two more public fields).

## License

GPL-3.0-or-later.
