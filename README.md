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
snapshot part (§19.10.3) and does not open the parts before it; `verify` reads every part.
`scrub` and `clear` refuse parts that declare a spec version newer than tsvz implements (§6.3).

Loose files (`.tsv .csv .nsv .psv`, §20.2.4) are read as TSVZ.py's command line reads them
(`_processLine`): keys and cells without the whitespace at their end and decoded (`<sep>`, `<LF>`),
`#_defaults_#` lines that fill the empty cells after them, the last line read also without its
newline, and no row for a missing key. A key you name is the cell a reader reads from it as written:
in a tab file `$'a\n'` and `$'a\t'` name only the row whose cell holds them, `a<LF>` and `a<sep>`, so
with only `a` in the file they name no row (TSVZ.py's `get` reaches `a`:
[docs/silent-choices.md](docs/silent-choices.md)); a blank at its end goes. Writes encode cells as
TSVZ.py's do, and refuse a line that would not read back as its cells (a key such as `#tag`,
a cell holding `</sep/>`), saying why; a `delete` of a key no row can have writes nothing and is
no failure. An append ends an unterminated last line first, in a
compressed file too (TSVZ.py's joins the compressed one's). A rewrite writes each
line it keeps as the file has it, but for three things: the key a one-column `delete` puts where the
line that set the column count went, and the empty line 1 and blank `#_defaults_#` line a `clear`
may write before the defaults it keeps. `scrub` keeps the lines a read depends on, for both command lines and
for TSVZ.py's library reads, strict or not, given no header or one (a key's last line may take the place of its
first where each of them reads the key from those two lines), and `clear` the `#_defaults_#` lines in
force ([docs/silent-choices.md](docs/silent-choices.md)).

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
socket's mode. Rust programs talk to a handler with `tsvz::handler::Client`. On a store on a cifs
(SMB) mount from an older Linux client (6.1 and RHEL 8's 4.18 were measured; 7.0 is not affected), the other commands cannot read the handler's pointer
file: they warn `ignoring the pointer file` and work on the files, and `tsvz stop` fails: stop the
handler with a signal or let `--x-idle-timeout` do it (deferred until TSVZ.py's next version,
[docs/limitations.md](docs/limitations.md)).

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

Appends take the same kind of file lock as TSVZ.py (an open-file-description lock on Linux,
`flock` on macOS), and the two exclude each other. Where the system refuses it (Linux before
3.15, or a file system without it), it is a classic POSIX lock, which a multi-threaded process
(`tsvz serve`, or a program that reads while it appends) loses for a moment when one thread
closes another descriptor of the file: a record it or a TSVZ.py writer appends then could be
lost ([docs/limitations.md](docs/limitations.md)).

Build without the command line and C libraries with
`default-features = false, features = ["bzip2"]` (gzip and all digests are always in).

## Conformance and compatibility

tsvz-rs passes the reader, writer, cli and handler classes of the TSVZ conformance suite with
every codec, every digest, multi-part scrub and the snapshot skip claimed, and is checked
against TSVZ.py on random stores, with and without its handler. Rust and Python writers can
share a store: their file locks exclude each other. Where the specification leaves a choice open,
tsvz-rs does what TSVZ.py does, except where [docs/silent-choices.md](docs/silent-choices.md), which
lists those choices, says it departs. Benchmarks against TSVZ.py are in [bench/RESULTS.md](bench/RESULTS.md).

## Upgrading to 0.2.4 from 0.2.3

Stores need nothing; the library's documented API does not change.

- Loose files: one that `set`, `append`, `setdefault` or `delete` creates begins with an empty
  line 1, as TSVZ.py's do (0.2.3 wrote the first row there, which a TSVZ.py program that reads a
  header took for one). `scrub` keeps what TSVZ.py's strict library read, and its reads given a
  header, read: their rows and the order of their keys (0.2.3 could change the strict read's rows
  under a `#_defaults_#` line that holds a value, the order of its keys, and what a read given a
  header read); a key that they read from other lines than the command line does keeps those
  lines, so a scrubbed file can be a few lines longer than 0.2.3 left it.
- Through a handler: a `pop` whose answer a stop cuts off (its client did not read it) gets its
  row put back on the files before `serve` ends; on macOS that cut now happens (0.2.3's stop waited
  for good for a client that did not read its answer); while writes fail, a request the handler
  cannot parse is warned of it too; a `serve` that takes over from a handler that was killed
  removes the socket directory it left.

## Upgrading to 0.2.3 from 0.2.2

Nothing changes in behaviour, stores or the library's API. 0.2.2 did not build for the musl targets
(its release has no Linux binaries, and `cargo install tsvz` fails on Alpine); 0.2.3 builds for them.

## Upgrading to 0.2.2 from 0.2.1

Stores need nothing; the library's documented API does not change.

- Loose files: an append to a compressed one that holds something begins with a newline, so its
  last line is never joined (TSVZ.py's append joins it); `setdefault` of a missing key prints the
  row `get` then reads (none, exit 3, where the line it wrote is a deletion); `clear` writes an
  empty line 1 where line 1 was no comment, as TSVZ.py's does (TSVZ.py's also replaces a comment
  line 1, which tsvz-rs keeps); `scrub` keeps the rows TSVZ.py's strict reader reads too (not always
  their order), where no `#_defaults_#` line holds a value ([docs/limitations.md](docs/limitations.md)); `scrub` and
  `clear` take a file named `app.tsv.1` as one file. Reads that
  want few rows (`get`, `len`, `keys`) take less CPU, and a row holds only its own cells.
- Through a handler: `tsvz stop` of a handler killed after it answered, or that closed one connection
  for want of a thread, ends at once (it waited 30 s); a stop that finds an acknowledged write lost answers 1
  (`tsvz stop` waits for the handler to go, then exits 1), and `serve` ends with status 1; a
  request that sets `#_write_ack_#` is answered once its own writes are synced; a `pop` whose
  client went away before its answer gets its row put back, while the handler serves.

## Upgrading to 0.2.1 from 0.2.0

Stores need nothing; the library's documented API does not change.

- A read of a multi-part strict store begins at its last snapshot part (§19.10.3) and does not
  open the parts before it, as TSVZ.py's does: a part there that is damaged under
  `#_on_corruption_# stop` no longer stops the read. `verify`, `scrub` and `clear` still read
  every part.
- Loose files are read as TSVZ.py reads them: a key or cell loses the whitespace at its end
  (`a ` and `a` are one key), `<sep>` and `<LF>` in a cell are the delimiter and a newline,
  `#_defaults_#` lines fill the empty cells after them, a line whose key is only whitespace is
  no row, the last line is read even without its newline, and `get` of a missing key prints
  nothing (it still exits 3). `set` writes a delimiter or newline in a cell as `<sep>` or `<LF>`
  (0.2.0 refused it), refuses a line that would not read back as its cells (a key such as
  `#tag`, a cell holding `</sep/>`), and ends an unterminated last line of a plain file with a newline
  before it appends (0.2.0 cut that line). A key typed with a trailing
  line break, or in a tab file a trailing tab, names only the row whose cell holds it: `$'b\t'` no
  longer reaches row `b` in a tab file (0.2.0's `delete` removed `b` in a file of one column;
  TSVZ.py's `get` reaches it). `set -` and `delete -` take the `#_defaults_#` line as `set` and
  `delete` of it do (0.2.0 wrote nothing for either).
- A `delete` in a compressed loose file, and a loose `scrub` or one-column `delete`, use far
  less memory.
- Through a handler: `tsvz stop` exits 0 when another stop or a signal stopped the handler first
  (it exited 1, "does not answer" or "went away"), and exits 1 when the handler is still there
  30 s after the request (it waited 10 s and exited 0 either way). A stopping handler no longer
  cuts off a `pop` or `popitem` that waits for its write, even past the stop's 5 s; until it has
  written its queued writes, a command that reaches it finds it gone (a write exits 1, a read uses
  the files) instead of working on the files beside it. A routed `pop` whose row cannot be
  printed is put back on the files when the handler cannot put it back, once the handler has
  stopped (a handler that still serves the store leaves the files alone: the row is printed on
  standard error). While a `#_write_ack_#`
  the handler queued is not yet written, every write through it is answered once written and
  synced. A read that falls back from a lost handler no longer warns that it "may or may not have
  been applied", and a command that meets a handler just as it stops no longer warns of a hard
  link.
- A `scrub` or `clear` whose part another scrub renames away while it waits for the part's lock
  reads the store again, instead of rewriting the renamed file.

## Upgrading to 0.2.0 from 0.1.4

The library changes, so a `0.1` requirement does not take 0.2.0: `tsvz::lock::Handle` is gone
(open a store's files with `std::fs::File`), and `tsvz::maint::snapshot` is no longer public
(`scrub` writes snapshots). Of the `#[doc(hidden)]` internals, `lock::lock` takes the file alone
and `Writer::append_runs_at` is now `append_at`.

- On macOS appends take `flock`, as TSVZ.py 5.3 and later do, instead of a classic POSIX lock;
  the two exclude each other on a Mac's local file system, so 0.1.4 and 0.2.0 can write one
  store there while you upgrade (across a network mount this was not tried).
- A routed command that reaches a handler as it stops no longer gets `the handler is stopping`.
  A request the handler had received before the stop is served and answered, for up to 5 s (on
  Linux this includes one it had not yet read; macOS drops that one). For any other the handler
  closes the connection: `read`, `get`, `has`, `len` and `keys` run on the files and succeed
  (before, they exited 1), and any other routed command exits 1 saying the request may or may
  not have been applied. A command that comes once the handler has closed its socket works on
  the files, with a warning, as before.
- A compressed batch that sets `#_member_size_#` is written at the member size in force where
  it started. A handler applies the new size from its next batch, and so do `pop`, `popitem`
  and `setdefault` on the files; `set`, `append` and `delete` on the files write at 1 MiB, as
  before (TSVZ.py writes a `delete` of a key it strips at the size in force; only the members'
  layout differs). Reads are unchanged.
- A scrub tried again because a writer appended meanwhile keeps every `.damaged-` copy it made.

## Upgrading to 0.1.4 from 0.1.3

In strict stores (`.tsvz .csvz .nsvz .psvz`) a read replays every part, as before; a multi-part
`scrub` writes snapshot parts that carry a `#_snapshot_#` line (the first, or the second after a
header comment; it also makes one marked copy of a part a 0.1.3 scrub wrote), and a writer now
refuses `#_snapshot_#` (§19.10.2; 0.1.3 accepted it and wrote it as a record). Upgrade every host
that scrubs the same multi-part store: a 0.1.3 scrub after a 0.1.4 one writes a new snapshot
part each time, or compacts in place when no ordinal is free (0.1.3 does not expect the marker
line), and the next 0.1.4 scrub marks it: with a marked copy while an ordinal is free, otherwise in
place, with the §19.8 warning.

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
- `scrub` keeps each line it keeps as the file has it, `#_defaults_#` lines included, and an
  unterminated last line (0.1.3's `scrub` dropped it); `clear` keeps the `#_defaults_#` lines in
  force, so that rows written later read to TSVZ.py with the defaults they had (a file of one
  column keeps none: rows written there later read without them).

The library's documented API does not change (of the `#[doc(hidden)]` internals, `codec::member`
and `codec::Member` are gone, and `frame::Line` has one field more, `bom`).

## License

GPL-3.0-or-later.
