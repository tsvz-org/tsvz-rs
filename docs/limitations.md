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
  before it appends. tsvz-rs reads it as an uncommitted tail, as §4.3 says of a part (§5.2 gives a
  loose file no further guarantee): `read` skips it with a warning, and `set`, or a `delete` in a
  file of more than one column, cuts it first ("cut N uncommitted bytes"), so that row is lost. A
  loose `scrub`, and a delete that rewrites a one-column file, keep it as it is (the scrub's warning
  says "ignored": its read ignores the bytes, and the file keeps them), so TSVZ.py goes on reading
  a row there that tsvz-rs does not list. A one-column delete picks the key that takes the place of
  a column-count line that goes without seeing that row, so `a`, then `b<TAB>1` with no newline,
  after `delete a`, reads two columns to TSVZ.py (`b<TAB>1`), where its own delete writes `b`, a
  newline and `b<TAB>1` (one column), and a key alone written later is then a deletion to it. A
  byte-order mark in front of the line, in a file with no other line, is dropped by a scrub. A
  loose `clear` drops it with the rows, and so a line 1 that is a comment and has no newline.
- Why: tsvz-rs applies the strict-store tail rule to loose files; changing it touches how a store
  follows appends (`Store::refresh` holds the tail until a newline completes it). Found in 0.1.4's
  differential runs against TSVZ.py 5.18; the other differences are in the entry below.
- Revisit when: the next tsvz-rs version. The plan: read a loose file's tail as a line, and write a
  newline before an append instead of cutting it, as TSVZ.py does.
- Noted: 2026-11-15, in the 0.1.4 build; a deferral under the user's standing rule that deferrals
  are noted, not asked.

### Loose reads still differ from TSVZ.py's in six ways
- Kind: deferred
- What the operator sees: in a loose file, `tsvz read`, `get` and `keys` differ from TSVZ.py's command line in
  six ways, none of them new in 0.1.4:
  - the values of a `#_defaults_#` line are not filled into the empty cells of the rows after it (tsvz-rs reads
    the line as a comment);
  - a bare `#_defaults_#` line in a one-column file is a row to TSVZ.py, and no row to tsvz-rs;
  - a key is not stripped of the whitespace at its end or decoded (`<sep>`, `<LF>`): `a ` and `a` are two keys
    to tsvz-rs, and one to TSVZ.py; nor is a cell, which keeps its trailing whitespace (a CR among it);
  - a line whose key is only whitespace is a row to tsvz-rs, and none to TSVZ.py (`delete` writes such a key as
    given, so that `popitem` drains a file that holds one);
  - TSVZ.py cuts every CR before a line's newline, tsvz-rs one (it keeps the rest in the last cell);
  - a row longer than the file's column count keeps the empty cells at its end, which TSVZ.py cuts (`a<TAB>1`,
    then `b<TAB>2<TAB><TAB>`: TSVZ.py reads `b<TAB>2`, tsvz-rs `b<TAB>2<TAB><TAB>`).
- Why: tsvz-rs reads a loose file by its own simple rule (§20.2.4 leaves the data semantics to the implementation);
  0.1.4 brought the column count (and with it the width rows are padded to), deletions, rewrites and keys alone in
  line with TSVZ.py's, which was what the user asked for ("Fix both in 0.1.4"), and left the read of cells and keys
  as it was.
- Revisit when: the next tsvz-rs version. The plan: port TSVZ.py's line rule (`_processLine`, with `_unsanitize`
  and `_fitRow`) as one function for loose reads, with TSVZ.py as the oracle.
- Noted: 2026-11-16, in the 0.1.4 final review; a deferral under the user's standing rule that deferrals are
  noted, not asked.

### A loose delete beside a clear can leave the key as a row
- Kind: deferred
- What the operator sees: `tsvz delete f.tsv k` running at the same moment as `tsvz clear f.tsv`
  can leave `k` in the cleared file as a row with no cells. The delete read the file's first
  keyed line, saw more than one column, and then appended `k` alone, which in the file as the
  clear left it, with no keyed line, sets one column and is a row. 0.1.3 read a key alone as a deletion at
  any column count, so that leftover `k` did not show; tsvz-rs 0.1.4 lists it.
- Why: the delete reads the first keyed line, then appends under the lock without checking that the
  file is still the one it read. The window is the time between the two.
- Revisit when: the next tsvz-rs version. The plan: append only while the file still has the stamp
  it was read with, and read again otherwise, as the one-column rewrite already does.
- Noted: 2026-11-16, in the 0.1.4 final review; a deferral under the user's standing rule that
  deferrals are noted, not asked.

### A read replays every part, also those before a snapshot part
- Kind: deferred
- What the operator sees: a read of a multi-part strict store replays every part, as in 0.1.3, however much of
  the store lies before its last snapshot part. TSVZ.py begins at that part and skips the ones before it
  (§19.10.3 lets a reader do so, it does not oblige it). On a store that follows §19.10.2 the result is the
  same, and only the time differs: 20 gzip parts of history before a snapshot part read in 0.19 s, where a read
  that begins at the snapshot part takes 0.02 s. On a store whose snapshot part does not restate the parts
  before it, which a producer must not write, tsvz-rs reads every part and TSVZ.py only from the snapshot part.
  A `scrub` writes the marker and finds a part marked.
- Why: finding the part before the read took a look at the start of every part, apart from the read. In
  0.1.4's reviews that look cost a second decode (bzip2 parts 1.3 to 1.9 times 0.1.3's time; a snapshot part of
  one 64 MB member 1.6 times with gzip and 2.5 times with xz), and leaving bzip2 out of it made the answer on
  such a store depend on the codec.
- Revisit when: the next tsvz-rs version. The plan: the read judges a part from its first lines as it decodes
  them, one rule for every codec, and a look before the read only chooses where to begin, from each part's
  first page; the read of that part confirms it, or the store is read again from part 0.
- Noted: 2026-11-16, in the 0.1.4 fix rounds; a deferral under the user's standing rule that deferrals are
  noted, not asked.

### A loose scrub holds every line of the file in memory
- Kind: deferred
- What the operator sees: `tsvz scrub` of a loose file uses memory that grows with the file's length, about 3.5
  to 6 times its size, also when most of its lines are old values of a few keys, where 0.1.3 needed memory for
  the live rows only: 200k lines of 1k keys (11.7 MB) peaked at 36 MB (0.1.3: 16 MB); 800k lines of 200k keys at
  168 MB (0.1.3: 127 MB). On files of mostly unique keys it uses less than 0.1.3 (228 MiB: 1.3 GB against 2.3 GB).
- Why: the scrub keeps each line it keeps as the file has it, so it holds every line, each in its own buffer,
  until it decides which go.
- Revisit when: the next tsvz-rs version. The plan: scrub in two passes. The first keeps a kind and a key number
  for each line, and each key's latest line; the second streams the file and writes the kept lines. Memory then
  follows the keys, as 0.1.3's did.
- Noted: 2026-11-16, in the 0.1.4 fix rounds; a deferral under the user's standing rule that deferrals are
  noted, not asked.

### A loose delete reads a compressed file whole
- Kind: deferred
- What the operator sees: `delete`, `pop` and `popitem` in a compressed loose file of more than one column read
  the whole compressed file into memory and decode it up to its first keyed line: on 88 MiB of gzip, 0.06 s and
  91 MB in 1 MiB members, 0.47 to 0.55 s and 346 to 575 MB as one large member (0.1.3: 0.00 s, 15 MB). A plain
  file is read only as far as its first keyed line.
- Why: the read decodes a compressed part as one slice, where a damaged member's bytes run to the next member's
  start, found by looking ahead; a separate streaming look at the first lines read past a damaged member into the
  next one (0.1.4's third review), so it was removed.
- Revisit when: the next tsvz-rs version. The plan: decode a part from the file a member at a time; the delete's
  read then stops as a plain file's does, with no second decoder.
- Noted: 2026-11-16, in the 0.1.4 fix rounds; a deferral under the user's standing rule that deferrals are
  noted, not asked.
