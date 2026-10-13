# tsvz-rs: core crate and command-line tool

## Intent

A fast, standalone implementation of tsvz-spec-v1 in Rust: a `tsvz` binary
that does what TSVZ.py's `tsvz` does for spec stores, much faster and without
Python, built on a library crate that other Rust programs can embed.

This is the first of three sub-projects. It delivers the library (reading,
writing, single-part maintenance) and the §20 command-line tool. The handler
(§21, sub-project 2) and the multi-part scrub (§19.2, sub-project 3) build on
it later, each with its own design.

In this document `§N` refers to tsvz-spec-v1 (`../TSVZ/tsvz-spec-v1.md`);
"section N" refers to this design's own sections. "TSVZ.py" is the Python
reference implementation in `../TSVZ/TSVZ.py`.

### Decisions taken during design

| Question | Decision |
|---|---|
| Purpose | A fast standalone `tsvz` binary; its engine is also a library crate |
| Split | Three sub-projects: core crate + CLI (this one), handler, multi-part scrub |
| Optional features claimed here | `bzip2`, `xz`, `zstd`, `crc32`, `crc32c`, `sha256`, `blake3`, `file-first`; `handler` and `scrub-multipart` come later |
| CLI scope | §20 only: no 3.39 rules for loose files, no `--x-` options |
| Repository and names | New repo `cas/tsvz-rs`; crate `tsvz`; binary `tsvz` |
| Distribution | crates.io (`cargo install tsvz`) and prebuilt binaries on GitHub releases; not on PyPI |
| Codec libraries | Pure Rust for gzip (`flate2` + `zlib-rs`) and bzip2 (`libbz2-rs-sys`); the reference C libraries for xz (`liblzma`) and zstd (`libzstd`), linked statically |
| Platforms | Linux and macOS |
| Speed bar | Faster than TSVZ.py on every tsvzbench `cold`, `bulk`, `perop` CLI and `maint` workload, numbers recorded |
| License | GPL-3.0-or-later |
| Engine structure | Layered: framer → replay state machine → owned ordered store with incremental refresh, shared by the CLI and the later handler |
| Interoperability | Same files, same locks as TSVZ.py; where the spec is silent, behave as TSVZ.py does and record the choice |

### Out of scope

- The handler: `serve`, `stop`, forwarding commands to a handler (§20.8, §21).
- Scrubbing a multi-part store (§19.2 snapshot into a new part); it is refused.
- TSVZ.py's 3.39 rules for loose files and its `--x-` options.
- Windows.
- A Python package, and bundling in cas-toolbox.

## 1. Repository and crate

### 1.1 Layout

```
tsvz-rs/
  Cargo.toml            one crate, `tsvz`: library + `tsvz` binary
  src/lib.rs            the library's public API (section 2.2)
  src/{name,parts,codec,frame,escape,digest,replay,store,write,lock,maint}.rs
  src/cli/              argument parsing, operations, output (feature `cli`)
  src/main.rs           the binary: calls cli::main
  tests/                integration tests (section 7)
  fuzz/                 cargo-fuzz targets (section 7.4)
  difftest/             the differential harness against TSVZ.py (section 7.3)
  bench/RESULTS.md      recorded tsvzbench numbers (section 8)
  docs/silent-choices.md  behaviour the spec leaves open, and what we chose (section 6.2)
  docs/superpowers/     specs and plans
```

### 1.2 Crate settings

- Edition 2024, stable toolchain; the minimum supported Rust version is the
  first stable release that supports edition 2024 (1.85).
- `license = "GPL-3.0-or-later"`, version `0.1.0` for the first release.
- Features: `cli` (the binary and its parsing), `bzip2`, `xz`, `zstd`, all in
  `default`. gzip and the four digests are always built. A library user who
  does not want C code sets `default-features = false` and enables `bzip2`
  (pure Rust) only.
- `[[bin]] name = "tsvz", required-features = ["cli"]`, so `cargo install tsvz`
  builds the tool and a library dependency does not.
- Release profile: `lto = "fat"`, `codegen-units = 1`, `panic = "abort"`.

### 1.3 Dependencies

| Need | Crate |
|---|---|
| Line and byte scanning | `memchr` |
| gzip (raw deflate; the gzip framing is ours) | `flate2` with the `zlib-rs` backend |
| bzip2 | `bzip2` on `libbz2-rs-sys` |
| xz | `liblzma` with static linking |
| zstd | `zstd` with static linking |
| Digests | `crc32fast`, `crc32c`, `sha2`, `blake3` (pure build) |
| Hash map | `hashbrown` |
| Locks, signals, terminal size | `libc` |

Table widths (section 5.4) use a small table generated from the Unicode
Character Database and checked into the crate, so they follow TSVZ.py's rule
rather than a width crate's.

There is no argument-parsing crate: §20.1.2's rules are easier to meet exactly
with a small hand-written parser (section 5.1).

## 2. Modules and the library API

### 2.1 Modules

| Module | Job | Spec |
|---|---|---|
| `name` | Variants and delimiters, extensions, the part filename grammar, codec suffixes | §5, §16.2, §17.2, App. A |
| `parts` | A store's parts in replay order; `.rotated` excluded | §17 |
| `codec` | Decoders and encoders; splitting a part into members; judging damaged members | §16 |
| `frame` | Committed lines with their part and offset: BOM, CRLF, commit rule, zero fill | §4 |
| `escape` | §13 token encoding and decoding | §13 |
| `digest` | crc32, crc32c, sha256 and blake3 accumulators | §15.2.1 |
| `replay` | The §7 pipeline as a state machine | §6–§15 |
| `store` | The ordered map of live rows, the missing-key result, refresh | §3, §14 |
| `write` | Record encoding; appends to plain and compressed parts | §13.3, §16.4, §16.7, §18.2 |
| `lock` | The write lock shared with TSVZ.py | §18.6 (as TSVZ.py implements it) |
| `maint` | `clear` and single-part `scrub` in place | §19 |
| `cli` | §20 | §20 |

Each module is unit-tested on its own (section 7.1). Dependencies point one
way: `frame` uses `codec`; `replay` uses `frame`, `escape` and `digest`;
`store` uses `replay` and `parts`; `write`, `maint` and `cli` use the rest.

### 2.2 Public API

```rust
// Opening and reading
pub struct Options {
    pub scope: Scope,                 // what the store keeps (section 3.6)
    pub verify: bool,                 // compute digests (section 3.5); off for plain reads
    pub delimiter: Option<u8>,        // loose files only (section 5.6)
    pub warnings: Option<Arc<dyn Fn(&Warning) + Send + Sync>>,
}
pub enum Scope { All, KeysOnly, Keys(Vec<String>) }

pub struct Store { /* section 3.6 */ }
impl Store {
    pub fn open(path: impl AsRef<Path>, options: Options) -> Result<Store, Error>;
    pub fn get(&self, key: &str) -> Option<Row<'_>>;      // a live key's resolved row
    pub fn lookup_key(&self, requested: &str) -> Cow<'_, str>; // §7.7 resolution of a requested key
    pub fn missing(&self) -> Option<Row<'_>>;             // §14.5: the current defaults, or None
    pub fn iter(&self) -> impl Iterator<Item = (&str, Row<'_>)>; // first-appearance order
    pub fn first(&self) -> Option<(&str, Row<'_>)>;
    pub fn last(&self) -> Option<(&str, Row<'_>)>;
    pub fn len(&self) -> usize;
    pub fn stopped(&self) -> Option<&Damage>;             // a stop policy ended the read (§4.10)
    pub fn mismatches(&self) -> &[Mismatch];              // with Options::verify
    pub fn markers(&self) -> &Markers;                    // final marker state
    pub fn parts(&self) -> &[PartInfo];
    pub fn refresh(&mut self) -> Result<Refreshed, Error>; // section 3.7
}
pub struct Row<'a> { /* cells + binding; resolves defaults on access */ }
impl Row<'_> { pub fn cells(&self) -> impl Iterator<Item = &str>; pub fn to_vec(&self) -> Vec<String>; }

// Writing
pub enum Record { Data { key: String, values: Vec<String> }, Tombstone { key: String },
                  Marker { key: String, values: Vec<String> } }
pub struct Writer { /* section 4 */ }
impl Writer {
    pub fn open(path: impl AsRef<Path>) -> Result<Writer, Error>;   // the store's active part
    pub fn member_size(self, bytes: usize) -> Writer;              // §16.7; default 1 MiB
    pub fn fsync(self, on: bool) -> Writer;
    pub fn append(&mut self, records: &[Record]) -> Result<(), Error>; // one batch, one write
}
pub fn classify_key(key: &str) -> KeyKind;   // Marker if it matches §12.2.1, else Data

// Maintenance
pub mod maint {
    pub fn clear(path: &Path, options: &Options) -> Result<Outcome, Error>;
    pub fn scrub(path: &Path, options: &Options) -> Result<Outcome, Error>;
}

// Streaming, for programs that want lines rather than a store: frame::Framer turns bytes into
// committed lines, replay::Replay applies them to any replay::Sink.
```

The supporting types:

| Type | Holds |
|---|---|
| `Error` | A missing store, an I/O failure (with the path), an unsupported codec, a path with no variant extension, or a refused operation with the reason (a multi-part `scrub`, a store that kept changing during maintenance, a loose field that cannot be written) |
| `Warning` | The part path, the byte offset when there is one, and a message saying what was found and what was done |
| `Damage` | Where a `stop` policy ended the read: part path, byte offset, and whether it was zero fill or a corrupt member |
| `Mismatch` | One failed segment: part path, line number in that part, algorithm, expected and computed digest |
| `Markers` | The final value of every marker of §12.4 except the checksum markers |
| `PartInfo` | Index, ordinal as written (none for the unnumbered part), path, codec, whether it is the active part |
| `Refreshed` | `Unchanged`, `Followed` (appends replayed) or `Reloaded` |
| `Outcome` | `Written`, `Unchanged` (identical bytes, nothing written), or `Partial` (the read stopped at damage; the part was rewritten from what was read and its original kept). A refusal is `Error::Refused`, with nothing written |
| `KeyKind` | `Marker` or `Data` |

A `stop` policy is not an `Error`: the store keeps what it read and reports it
through `stopped()` (section 3.8).

## 3. The read path

### 3.1 Parts (`parts`, §17)

The store path names the unnumbered file. Its numbered parts are the
directory entries `<base>.<format-ext>.<hex>[.<codec>]`, without `.rotated`,
ordered by the integer value of the ordinal; the unnumbered file, when it
exists, is part 0 before all of them. Codec suffixes are matched
case-insensitively against §16.2's list. Each part is read in 1 MiB blocks.

### 3.2 Compressed members (`codec`, §16.6)

A compressed part is read as a sequence of members. For each member:

1. Decode it from its start. A member is **complete** when the decoder reaches
   its end and its check passes; its output is used.
   - gzip: the crate parses the RFC 1952 header itself, runs raw deflate
     (`flate2::Decompress`), then reads the 8-byte trailer and compares the
     CRC-32 (`crc32fast`) and the length.
   - bzip2, xz, zstd: the streaming decoders report stream end, need-more-input
     and data error separately, with the bytes consumed. A zstd frame's content
     checksum is verified when it has one (§16.8).
   - The member's output is held until it is complete (the writer keeps members
     to 1 MiB; a foreign larger member is held whole).
2. A member that is not complete is **damaged**. Its bytes run to the next
   offset after its start where the codec's magic number begins, or to the end
   of the part. For gzip, zero bytes ending that region are padding except those
   the member's own compressed data needs to reach its end and those that
   complete a trailer it has begun (§16.6 rule 2).
3. The damaged bytes are decoded again on their own. If they merely end before
   the member does, the member is **cut short** and its output is used. If
   decoding fails, the member is **corrupt**: the corruption policy in force at
   its first byte decides (`recover` uses the output decoded before the
   failure, `skip` none, `stop` ends the read).
4. Bytes after the last `\n` before the next member are dropped, as at the end
   of a part, and reading continues at the next member.

Zero bytes before a gzip member are skipped. The runner's oracle in
`../TSVZ/tsvzconform.py` (`decompress_items`, `_gzip_unpad`, `_decode_alone`)
is the line-by-line reference for these rules.

### 3.3 Framing (`frame`, §4)

The framer turns a part's bytes (or a compressed part's member output) into
committed lines, each with its part, its 1-based line number in that part and
its byte offset:

- One UTF-8 byte-order mark is removed at the start of a part; in a compressed
  part only when no damaged member came before it (§4.7). It is not part of the
  first line and not fed to digests.
- Lines end at `\n` (`memchr`); one `\r` before it is removed for parsing but
  kept in the line's raw bytes for digests (§4.2, §15.4).
- Bytes after the last `\n` of a part are not a line (§4.3).
- In a `.tsvz`, `.csvz` or `.psvz` part, a line holding a zero byte is zero-fill
  damage (§4.9): under `recover` the line is the bytes after its last zero
  byte, under `skip` it is ignored, under `stop` the read ends. Digests get the
  whole raw line, zeros included, whatever the policy does.
- Text is UTF-8; ill-formed sequences become U+FFFD, one per maximal subpart
  (§4.8), which is what Rust's lossy UTF-8 decoding does. A line that is valid
  UTF-8, the common case, is used without copying.

### 3.4 Replay (`replay`, §7)

For each committed line, in order:

1. Split on the raw delimiter.
2. Classify the raw first field: an integrity marker, an official marker, an
   unrecognized marker (§12.2.1 pattern, ASCII case-insensitive), a comment,
   or a data row.
3. Feed the line's raw bytes to every armed digest except the one whose marker
   this line is (section 3.5).
4. Apply official markers (§12.4): values are decoded, invalid values ignored
   (§12.4.5), an empty or missing value resets to the built-in default. The
   state is version, defaults, strip, fill-empty, return-defaults-when-missing,
   on-corruption, and the advisory rotate, write-ack and member-size values.
5. A data row with no raw delimiter is a tombstone (§7.6). The key is stripped
   of trailing space and tab while stripping is on, then token-decoded; an empty
   key ignores the row (§7.7). A tombstone deletes its key (§7.8).
6. Value cells are stripped and decoded only when they end in space or tab or
   contain `<`; otherwise the raw text is the value.
7. The row is stored with its **binding**: the defaults and fill-empty setting
   in force at its position (§12.4.4, §14.4). A binding is a reference-counted
   value replaced only when one of those markers changes, so rows between two
   marker changes share one.

### 3.5 Integrity (`digest`, §15)

Digests are computed only when `Options::verify` is set (the CLI's `verify`).
A plain read skips them: §15.1 leaves the action on a mismatch to the
implementation, and reads stay fast. When verifying, the first
`#_checksum_<algo>_#` for an algorithm arms its accumulator; a later one with a
value compares and records a `Mismatch` (part path, line number in that part,
algorithm in lowercase, expected and computed digest in lowercase hex), then
starts a new segment. Segments cross part boundaries (§15.4). All four
algorithms of §15.2.1 are implemented; any other name is inert.

### 3.6 The store (`store`)

- Live rows are a vector of slots in first-appearance order plus a
  `hashbrown` map from key to slot. A slot holds the key, the decoded cells in
  one buffer with their boundaries, and the row's binding.
- Deleting a key frees its slot; writing it again appends a new slot at the end
  (§3.4). Freed slots are compacted when they outnumber live ones.
- `Row` resolves on access: a present non-empty cell is its value; a present
  empty cell is the binding's default when its fill-empty is on, else empty; an
  absent cell is the binding's default (§14.2, §14.3).
- `missing()` is the final defaults when `#_return_defaults_when_missing_#` is
  true, else `None` (§14.5).
- `lookup_key` strips a requested key's trailing space and tab while the final
  strip state is on (§20.2 `get`).
- **Scope** limits what is kept, for speed: `All` keeps every row; `KeysOnly`
  keeps keys and order but no cells (`len`, `keys`, `has`); `Keys(list)` keeps
  rows only for the listed keys, plus the full marker state (`get`). Every scope
  replays every line, so tombstones and markers are applied exactly.

### 3.7 Refresh (`Store::refresh`)

A store kept open follows the files without re-reading what it has. For each
part it records the inode, size and modification time, and for the last part
the offset after its last committed line, the uncommitted tail, the replay
state at that offset, and a **check**: up to 4096 bytes just before that offset.
`refresh()` then:

1. Re-lists the parts when the directory's modification time changed, or while
   it is less than 2 seconds old (a directory can change again within one
   timestamp tick). A different part list means a full reload.
2. For each part whose inode, size or modification time changed:
   - a part other than the last, a compressed last part, or a store whose read
     stopped (§4.10): full reload;
   - the last part when it has the same inode, has grown, is at least as long as
     the recorded offset, and still holds the check bytes just before that
     offset: replay only the bytes after the offset;
   - anything else, including a part that shrank: full reload.

The check catches an in-place rewrite (`clear`, `scrub`, §19.8) followed by
enough appends to grow the part past the old offset, which size and inode alone
miss. TSVZ.py's handler uses the same rules with a 64-byte check. This is what
the handler (sub-project 2) needs for §21.8.1 and §21.8.5.

### 3.8 Failures and warnings

- A missing store, a part that cannot be opened or read, or a part in a codec
  the build does not include (§16.5) is an `Error`.
- A `stop` policy (§4.10) is not an `Error`: the store keeps what was read
  before the damage and `stopped()` says where. The CLI prints that and exits 1.
- Tolerated problems (an unterminated tail, zero fill under `recover` or
  `skip`, a cut-short or corrupt member, invalid UTF-8, an invalid marker
  value, a conflicting `-d`) are `Warning`s given to the callback, with the part
  and offset and what was done (§4.10's SHOULD).

## 4. The write path

### 4.1 Encoding (§13.3)

In a field, the variant's delimiter is written `<sep>`, line feed `<LF>`,
carriage return `<CR>`, the zero byte `<NUL>` (`<sep>` in `.nsvz`, where it is
the delimiter) and `<` as `<lt>`. A data key whose first character is `#` is
written with `<#>`. A key that matches the reserved pattern (§12.2.1) is a
marker key and is written as it is. A new file never starts with a byte-order
mark. Command-line arguments are literal strings (§20.1.4): they are encoded,
never token-decoded.

### 4.2 Appends (§16.4, §17.7, §18.2)

- Appends go to the store's last part in replay order, which is the unnumbered
  file when it is the only part. A missing store is created at the store path
  (§20.2.2).
- Plain part: open for append, take the write lock (section 4.3), and when the
  file does not end in `\n`, truncate the uncommitted tail so the batch cannot
  join it (as TSVZ.py does, with a warning). The whole batch is one `write()`.
- Compressed part: the batch becomes new members appended after the existing
  bytes, each with at most the member size of uncompressed records and ending
  at a record boundary; one record longer than that is a member of its own
  (§16.7). zstd frames set the content checksum flag (§16.8). Existing members
  are not read, checked or repaired.
- The member size is 1 MiB unless the caller passes the store's
  `#_member_size_#`. The CLI's `set` and `delete` do not replay the store, so
  they use 1 MiB, as TSVZ.py's command does; `Writer::member_size` lets a caller
  with an open `Store` (and later the handler) pass the marker's value.
- Appends are not fsynced unless asked (`Writer::fsync`); the commit rule makes
  a torn append harmless. Rewrites are always fsynced (section 4.4).
- `pop`, `popitem` and `setdefault` replay the store, then append; on the files
  they are not atomic with other writers (§20.2.5).

### 4.3 Locks (shared with TSVZ.py)

TSVZ.py does not use §18.6's two byte ranges. It holds one exclusive
whole-file lock for every append and every in-place rewrite, and readers take
none. tsvz-rs does the same, so Rust and Python writers exclude each other:

- a process-wide mutex keyed by the file's canonical path (POSIX record locks
  do not exclude two handles of one process);
- then, on Linux, an open-file-description lock over the whole file
  (`F_OFD_SETLKW`, `l_start = 0`, `l_len = 0`), falling back to `lockf` when the
  kernel refuses it (`EINVAL`); on macOS, `lockf`. Linux treats both kinds as
  one lock space, so any mix of the two tools excludes correctly.

No part is ever replaced by renaming another file over it (§18.7). `flock` is
reserved for the handler's pointer file (sub-project 2).

### 4.4 Maintenance (`maint`, §19)

`clear` and `scrub` of a one-part store:

1. Read the part, recording its inode, size and modification time.
2. Build the complete new content in memory.
3. Take the write lock and compare inode, size and modification time again. If
   another writer committed in between, release and start over, at most 3
   times; then fail with nothing written (§18.8).
4. If the new bytes equal the old, write nothing.
5. Otherwise write from offset 0, truncate to the new length, `fsync`
   (§19.8). A compressed part is written as members of the member size from the
   store's final `#_member_size_#`.

What `scrub` writes (§19.2.3, §19.4):

- the header comment, when the first committed line of the part is a comment;
- `#_version_#` with the value `1`; then, in this order and only when not the
  built-in default: `#_strip_trailing_whites_#` false, `#_fill_empty_with_default_#`
  true, `#_return_defaults_when_missing_#` false, `#_rotate_#`, `#_write_ack_#`,
  `#_member_size_#`, `#_on_corruption_#`; then `#_defaults_#` last, when any
  default is non-empty (TSVZ.py's order);
- one row per live key in first-appearance order, every row **fully
  resolved** (its defaults baked in) and padded with empty cells to the width
  of the final defaults, so no final default fills a column the row was never
  bound to;
- when some row would not read back the same under the final marker state (a
  key or cell ending in space or tab while stripping is on, or an empty cell
  whose final default is non-empty while fill-empty is on), the compensating
  layout of §19.4 instead: `#_strip_trailing_whites_#` false and
  `#_fill_empty_with_default_#` false in place of their usual lines before the
  rows, and after the rows `#_strip_trailing_whites_#` true and
  `#_fill_empty_with_default_#` true again where the final state has them.

When the read met a `stop` policy or a corrupt member, the part's original
bytes are first copied to `<part>.damaged-<timestamp>` (`.1`, `.2`, ... when the
name is taken) and synced, as TSVZ.py does; after a `stop`, `scrub` still
exits 1 (§4.10).

`clear` of a one-part store rewrites it keeping only the header comment and
the marker lines `scrub` would write without the `#_version_#` line, as
TSVZ.py does. A read that stopped at damaged data refuses `clear` (§4.10: no
changes are based on such a read). The backup copy above is made whenever the
read met zero fill, or a member that was cut short or corrupt. `clear` of a multi-part store appends a
tombstone for every live key to the active part (§18.8). `scrub` of a
multi-part store is refused with exit 1 and nothing written (§20.2), until
sub-project 3.

## 5. The command-line tool (`cli`, §20)

### 5.1 Arguments (§20.1)

- Options may appear anywhere before an argument that is exactly `--`; after it
  every argument is positional. An argument that is `-`, or `-` followed by a
  digit or `.`, is positional. A long option's value may be given as
  `--option=VALUE`.
- The first positional argument chooses the form: an operation name of §20.2
  means `OPERATION STORE [ARG ...]`; anything else means
  `STORE [OPERATION] [ARG ...]`, where a missing operation is `read` (§20.1.5).
- Options: `-h`/`--help` (usage on stdout, exit 0), `-V`/`--version`
  (`tsvz-rs 0.1.0` on stdout, exit 0), `--format table|records`, `-q`/`--quiet`,
  `-v`/`--verbose`, `-d`/`--delimiter D` (one character or `tab`, `comma`,
  `pipe`, `null`; for a strict variant a conflicting value is overridden with a
  warning).
- An unknown operation or option, a missing or extra argument, or a bad option
  value exits 2 (§20.7.3).

### 5.2 Operations (§20.2)

Every operation of §20.2, with these notes:

- `read`, `popitem` and `clear`/`scrub` use `Scope::All`; `get`, `pop` and
  `setdefault` use `Scope::Keys` with their key; `len`, `keys` and `has` use
  `Scope::KeysOnly`.
- `get` resolves each requested key with `lookup_key`, prints a live key's row,
  prints the key and the current defaults for a missing key (nothing when
  `#_return_defaults_when_missing_#` is false), and exits 3 when any key was
  missing, after printing (§20.5.3).
- `set`/`append` write one record; a key with no value is a tombstone; an empty
  value is a present-empty cell; a marker key writes a marker line.
- `delete` writes one tombstone per key and never fails because a key is absent.
- `verify` prints one line per mismatch and exits 4 when there is any.
- `parts` prints index, the ordinal as written in the file name (empty for the
  unnumbered part), the path, and the flags `active` and the codec name,
  comma-separated.
- `pop`, `popitem [first|last]`, `setdefault` as §20.2 states.
- `serve` and `stop` exit 1 with a diagnostic until sub-project 2 (§20.2.6). The
  tool never reads pointer files.
- Missing stores: `set`, `append`, `delete`, `clear` and `setdefault` create the
  store; every other operation exits 1 without creating it (§20.2.2).

### 5.3 Bulk input (§20.3)

`set -` and `delete -` read standard input in the store's variant. Only
committed lines count; an unterminated last line is ignored with a warning.
Lines are framed and split as in section 3.3 and decoded per §13. For `set`,
each data line is one record (a lone key is a tombstone), a reserved-pattern
first field is a marker write, and comments and empty lines are skipped. For
`delete`, the first field of each non-empty line is a key, a reserved-pattern
first field resets that marker, and comments are skipped. All records go out as
one batch.

### 5.4 Output (§20.4, §20.5)

- `records`: one line per row, the store's delimiter, §13 encoding, `<#>` for a
  leading `#`. A row whose resolved cells are all empty is still printed with a
  delimiter, so it does not read back as a tombstone. `verify` and `parts` join
  their fields with TAB, each encoded per §13 for TSVZ.
- `table`, the default when stdout is a terminal (§20.4.3), is laid out as
  TSVZ.py lays it out:
  - Row output (`read`, `get`, `pop`, `popitem`, `setdefault`, and `keys` as
    one-column rows): no header; every row padded to the widest row's number of
    cells; each column padded to its widest cell; cells joined by ` | `; no cell
    is ever shortened; a row whose cells are all empty is drawn as the rule
    (`-` runs joined by `-+-`); the table is followed by one empty line.
  - `verify` and `parts`: a header row (`path line algorithm expected
    computed`, or `index ordinal path flags`) and a `-+-` rule, fitted to the
    terminal width: first with ` | ` between cells, then with `|`, then by
    shortening the columns that are wider than their header, widest first, a
    shortened cell keeping what fits and ending in `..`. No rows prints nothing.
  - `len` prints the number in both formats.
  - Before layout, every cell's control characters are escaped, so no stored
    value can move the cursor, reorder the line, or send the terminal an escape
    sequence: U+0000–U+001F, U+007F–U+009F, U+061C, U+200E, U+200F, U+2028,
    U+2029, U+202A–U+202E and U+2066–U+2069 are shown as `\t`, `\n` or `\r`
    for those three, else `\xNN` below U+0100 and `\uNNNN` above. A backslash
    is not escaped: the table is for reading, records are exact.
  - Widths count terminal columns by TSVZ.py's rule: a character below U+0300
    takes 1; a combining mark, a character of category Mn, Me or Cf, or one in
    U+1160–U+11FF takes 0; an East Asian Wide or Fullwidth character takes 2;
    anything else 1. The categories and East Asian widths come from a table the
    crate generates from the Unicode Character Database.
  - The terminal width is that of stdout (`TIOCGWINSZ`); when it cannot be
    read, `$COLUMNS`, else 240.
- Output goes through one buffered, locked stdout. Diagnostics go to stderr as
  `tsvz: <path>: <message>`; `-q` drops warnings, `-v` adds informational
  messages.
- SIGPIPE is reset to its default at start, so `tsvz read big | head` ends as
  other Unix tools do.

### 5.5 Exit status (§20.6)

0 done; 1 failed, refused, or the read stopped at damaged data (a `scrub` that
rewrote such a store included); 2 usage error; 3 a missing key for `get`, `has`,
`pop`, `popitem` as §20.2 states; 4 a checksum mismatch.

### 5.6 Loose files (§5.2, §20.2.4)

`.tsv`, `.csv`, `.nsv` and `.psv` files carry no spec semantics; tsvz-rs reads
and writes them by one simple rule. Committed lines are split on the delimiter
(the extension's, or `-d`), with no escaping, markers, stripping or integrity.
A line whose first character is `#` is a comment, a line with no delimiter
deletes its key, and the last record of a key wins. A value that contains the
delimiter or a newline cannot be written and is refused with exit 1. `scrub`
and `clear` rewrite the file in place keeping a header comment, `verify` has
nothing to check, and `parts` lists the file itself. TSVZ.py treats these files
by its 3.39 rules; the two tools may disagree on them, which §20.2.4 allows.

## 6. Interoperability with TSVZ.py

### 6.1 Shared files

Rust and Python tools may read and write the same store at the same time: the
locks of section 4.3 exclude their appends and rewrites, appends never rewrite
earlier bytes, and an open `Store` follows either tool's writes (section 3.7).
A store written by one reads identically in the other wherever the spec
decides the result.

### 6.2 Where the spec is silent

Where the spec leaves a choice open, tsvz-rs does what TSVZ.py does, so the two
tools agree in practice. Each such choice is listed in `docs/silent-choices.md`
with the TSVZ.py behaviour it copies. The differential harness (section 7.3)
finds new ones. The list starts with: the ordinal printed by `parts`; xz stream
padding and zstd skippable frames between members; the markers a `scrub` keeps;
what `scrub` does with checksum markers; truncating an uncommitted tail before
an append; the table layout of section 5.4; and the `-V` text.

## 7. Testing

### 7.1 Unit tests

Per module: token round-trips for every variant; the filename grammar and part
ordering (hex not lexical, leading zeros, uppercase, `.rotated`, part 0);
framing (BOM, CRLF, tails, zero fill under each policy, invalid UTF-8); member
judging with small fixtures for each codec (complete, cut short, corrupt, gzip
padding and begun trailers); the replay state machine (each marker, invalid
values, resets, bindings); store order, deletion and compaction; encoding; the
argument parser's §20.1.2 cases.

### 7.2 Conformance (the acceptance test)

```
python3 ../TSVZ/tsvzconform.py --impl target/release/tsvz \
  --class reader,writer,cli \
  --feature bzip2,xz,zstd,crc32,crc32c,sha256,blake3,file-first
```

must report no FAIL. It must report no warn either, except for a SHOULD case
that is deliberately not met and is listed, with the reason, in
`docs/silent-choices.md`. The `!handler` cases run too, and the CLI's
procedural checks (pointer refusal, terminal output, member size, zstd
checksum, `read | set -`).

### 7.3 Differential testing against TSVZ.py

`difftest/` generates random stores from a seed: markers (valid, invalid,
reset), tokens and bare `<`, CRLF and lone `\r`, zero fill, invalid UTF-8,
tombstones and empty keys, multi-part layouts, and gzip parts with damaged,
cut-short and padded members. For each it compares `read`, `get` of a present
and a missing key, `len`, `verify` and `parts` between tsvz-rs and TSVZ.py,
and, through a pseudo-terminal of fixed size, the table output of `read`,
`verify` and `parts` (control characters, East Asian and combining text
included). A difference is a bug in one of them, or a silent choice to record
(section 6.2).
It runs for a fixed seed range in CI and for longer locally.

### 7.4 Robustness

`cargo fuzz` targets feed arbitrary bytes to the framer and to each codec's
member splitter; they must not panic, hang, or allocate without bound.

### 7.5 Refresh and locking

- An integration test opens a `Store`, rewrites its part in place with `clear`,
  appends past the old size, and checks that `refresh()` reloads and shows the
  new state. Further tests cover a plain append, a new part, a removed part, a
  compressed last part and a stopped store.
- On Linux, a test runs Rust and TSVZ.py writers appending to one file at once
  and checks that every record is whole and none interleave.

## 8. Performance

- `../TSVZ/tsvzbench.py` runs with TSVZ.py as the baseline and
  `target/release/tsvz` as the candidate, over the `cold`, `bulk`, `perop` CLI
  and `maint` workloads. The handler workloads wait for sub-project 2.
- Runs use a work directory on `/mnt/gen5`, never tmpfs or mergerfs. Timings
  here can switch between a fast and a slow mode, so a small ratio is trusted
  only after repeated runs agree.
- `bench/RESULTS.md` records the commands, the machine, both versions and the
  table. The bar: faster than TSVZ.py on every workload. Where it is not,
  profile and fix that workload before release.
- Speed comes from: no interpreter start-up; `memchr` framing over large
  blocks; no copy for valid UTF-8 and no decode for cells without `<` or
  trailing whitespace; digests only for `verify`; replay scopes that keep only
  what an operation prints; `set` and `delete` that append without replaying.

## 9. CI and release

- GitHub Actions on Linux x86_64 and macOS arm64: `cargo fmt --check`,
  `cargo clippy --all-targets -- -D warnings`, `cargo test`, a release build,
  then the conformance command of section 7.2 against a checkout of
  `yufei-pan/TSVZ` under Python 3.14 (so the zstd cases run), and the
  differential harness for a fixed seed range.
- Release 0.1.0: `cargo publish` to crates.io after a code review of the whole
  crate; static musl binaries for Linux x86_64 and aarch64 and binaries for
  macOS arm64 and x86_64, attached to a GitHub release.
- After the release, as separate small changes: a Rust row in tsvz.org's
  implementations table (TSVZ repo) and a tsvz-rs row in `cas/CLAUDE.md`.

## 10. Done

- The conformance command of section 7.2 reports no FAIL, and no warn beyond
  those listed in `docs/silent-choices.md`, on Linux and macOS.
- The differential harness reports no unexplained difference over its CI seed
  range.
- `bench/RESULTS.md` shows tsvz-rs faster than TSVZ.py on every `cold`, `bulk`,
  `perop` CLI and `maint` workload.
- Unit, refresh, lock and fuzz tests pass; `docs/silent-choices.md` lists every
  choice made where the spec is silent.
- 0.1.0 is on crates.io and the GitHub release carries the four binaries.

## Follow-ups outside this sub-project

- Sub-project 2 (handler) builds on `Store::refresh`, `Writer::member_size`
  and the lock module.
- Sub-project 3 (multi-part scrub) builds on `maint::scrub`'s content builder.
- A handler conformance case in the TSVZ suite for an in-place rewrite followed
  by appends past the old size (the `write` step rewrites a file in place), so
  every handler is held to section 3.7's behaviour.
