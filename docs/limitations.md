# Limitations

Approved known limitations and deferrals. Reviewers: read this first, and don't raise these again.

### Rust readers take no lock, so they can see a TSVZ 5.3 rewrite part way
- Kind: deferred
- What the operator sees: TSVZ.py 5.3 readers of a `.tsv`-family file take a shared lock while they read (byte 0 of
  an open file description lock on Linux, `flock` `LOCK_SH` on macOS), so a rewrite under the file lock is waited
  out. tsvz-rs readers (`Handle::open_read` callers) take none: like TSVZ.py 5.2, they can read another process's
  in-place rewrite part way, and a delete made in that moment can be lost. Compatibility needs nothing on Linux:
  tsvz-rs's whole-file write lock conflicts with every 5.3 lock (its appends now also wait for Python readers,
  since byte 0 is inside its whole-file lock: slower, not wrong). On macOS `flock` and POSIX locks exclude each
  other (TSVZ's macOS CI job checked it, 2026-11-04, against `lockf`), so it holds there too.
- Why: tsvz-rs follows TSVZ.py; the change is a lock-layer change of its own:
  - `Handle::open_read` callers take READ: byte 0 shared on Linux, through the gate (byte 2 shared, let go of once
    byte 0 is held). On macOS that works only after a move to
    `flock`, because a shared POSIX lock would replace the process's own write lock.
  - `maint.rs` rewrites take byte 1, the gate (byte 2), then the whole file; appends move to byte 1, so they do not wait for Python
    readers.
  - Optional: `flock` on macOS would let the `Handle` table and its deferred closes go.
  - Tests: extend `tests/interop.rs` with "a Python READ waits for a Rust rewrite" and "a Rust reader waits for a
    Python rewrite".
- Revisit when: someone shares a file between tsvz-rs and another writer that rewrites it.
- Approved: 2026-11-04, the user chose "Deferred in tsvz-rs (Recommended)".

### A loose file's unterminated last line is ignored, and cut before an append
- Kind: deferred
- What the operator sees: a `.tsv` whose last line has no newline (a file saved by an editor or
  written with `printf`) shows that line in TSVZ.py, which reads it as a row and adds a newline
  before it appends. tsvz-rs reads it as an uncommitted tail, as §4.3 says of a strict store: `read`
  skips it with a warning, and `set`, or a `delete` in a file of more than one column, cuts it first
  ("cut N uncommitted bytes"), so that row is lost. A loose `scrub`, and a delete that rewrites a
  one-column file, keep it as it is, so TSVZ.py goes on reading a row there that tsvz-rs does not
  list; a byte-order mark in front of it, in a file with no other line, is dropped by a scrub. A
  loose `clear` drops it with the rows.
- Why: tsvz-rs applies the strict-store tail rule to loose files; changing it touches how a store
  follows appends (`Store::refresh` holds the tail until a newline completes it). Found in 0.1.4's
  differential runs against TSVZ.py 5.18; the other differences are in the entry below.
- Revisit when: the next tsvz-rs version. The plan: read a loose file's tail as a line, and write a
  newline before an append instead of cutting it, as TSVZ.py does.
- Noted: 2026-11-15, in the 0.1.4 build; a deferral under the user's standing rule that deferrals
  are noted, not asked.

### Loose reads still differ from TSVZ.py's in five ways
- Kind: deferred
- What the operator sees: in a loose file, `tsvz read`, `get` and `keys` differ from TSVZ.py's command line in
  five ways, none of them new in 0.1.4:
  - the values of a `#_defaults_#` line are not filled into the empty cells of the rows after it (tsvz-rs reads
    the line as a comment);
  - a bare `#_defaults_#` line in a one-column file is a row to TSVZ.py, and no row to tsvz-rs;
  - a key is not stripped of the whitespace at its end or decoded (`<lt>`, `<sep>`): `a ` and `a` are two keys
    to tsvz-rs, and one to TSVZ.py;
  - TSVZ.py cuts every CR before a line's newline, tsvz-rs one (it keeps the rest in the last cell);
  - rows are padded to the width of the first data row, where TSVZ.py pads to the first keyed line's, a deletion's
    included (`k<TAB><TAB>`, then `a<TAB>1`: TSVZ.py reads `a<TAB>1<TAB>`, tsvz-rs `a<TAB>1`); the cells that are
    written read alike.
- Why: tsvz-rs reads a loose file by its own simple rule (§20.2.4 leaves the data semantics to the implementation);
  0.1.4 brought the column count, deletions, rewrites and keys alone in line with TSVZ.py's, which was what the
  user asked for ("Fix both in 0.1.4"), and left the read of cells and keys as it was.
- Revisit when: the next tsvz-rs version. The plan: port TSVZ.py's line rule (`_processLine`, with `_unsanitize`
  and `_fitRow`) as one function for loose reads, with TSVZ.py as the oracle.
- Noted: 2026-11-16, in the 0.1.4 final review; a deferral under the user's standing rule that deferrals are
  noted, not asked.

### A loose scrub can change the empty cells tsvz-rs pads a row with
- Kind: deferred
- What the operator sees: in a loose file whose rows differ in length, `tsvz read` can show a row
  with more or fewer trailing empty cells after `tsvz scrub` than before
  (`#h\nk\t\na\t1\nb\t2\na\t3\t4\n`: `b\t2` before, `b\t2\t` after). The keys, their order and
  every written cell stay, and TSVZ.py reads the file exactly as before.
- Why: tsvz-rs pads rows to the width of the first data row, TSVZ.py to the column count of the
  first keyed line. The scrub keeps the first keyed line, but can put a longer line of the same
  key in the first data row's place. 0.1.3's scrub changed the padding in more files.
- Revisit when: the next tsvz-rs version. The plan: take a loose file's row width from its first
  keyed line, as TSVZ.py does (part of the entry above), so that one line decides both.
- Noted: 2026-11-16, in the 0.1.4 final review; a deferral under the user's standing rule that
  deferrals are noted, not asked.

### A loose delete beside a clear can leave the key as a row
- Kind: deferred
- What the operator sees: `tsvz delete f.tsv k` running at the same moment as `tsvz clear f.tsv`
  can leave `k` in the cleared file as a row with no cells. The delete read the file's first
  keyed line, saw more than one column, and then appended `k` alone, which in the file as the
  clear left it, with no keyed line, sets one column and is a row.
- Why: the delete reads the first keyed line, then appends under the lock without checking that the
  file is still the one it read. The window is the time between the two.
- Revisit when: the next tsvz-rs version. The plan: append only while the file still has the stamp
  it was read with, and read again otherwise, as the one-column rewrite already does.
- Noted: 2026-11-16, in the 0.1.4 final review; a deferral under the user's standing rule that
  deferrals are noted, not asked.

### A read of a store of many bzip2 parts decodes each part's first block twice
- Kind: deferred
- What the operator sees: reading a multi-part store with no snapshot part takes longer when its parts are
  bzip2: 40 parts of one 3 MiB member each, `tsvz get` took 3.9 s, and 0.1.3 1.7 s (gzip parts: the same as 0.1.3).
- Why: a read begins at the last snapshot part (§19.10.3), so each part, from the last, has its first lines
  looked at. bzip2 gives nothing before a whole block (up to 900 kB of text), so the look decodes that block, and
  the read then decodes it again. 0.1.3 had no such look.
- Revisit when: the next tsvz-rs version. The plan: keep the first member's output from the look for the read of
  that part, so that it is decoded once.
- Noted: 2026-11-16, in the 0.1.4 fix round; a deferral under the user's standing rule that deferrals are noted,
  not asked.
