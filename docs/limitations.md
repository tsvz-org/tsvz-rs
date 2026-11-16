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
  loose `clear` drops an unterminated last line with the rows, as 0.1.3's did, a comment or not.
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
  - a line whose key is only whitespace is a row to tsvz-rs when it holds a value (or in a one-column file), and
    a deletion of that key when it is alone or its cells are blank; TSVZ.py skips every such line (`delete`
    writes such a key as given, so that `popitem` drains a file that holds one);
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
  (§19.10.3 lets a reader do so, it does not oblige it). On a store that follows §19.10.2 the rows are the
  same, and the time differs: a `get` on 20 gzip parts of history (50,000 keys written 20 times), a snapshot
  part in 1 MiB members and a small active part takes 0.18 s (0.1.3: 0.18 s), where a read that begins at the
  snapshot part takes 0.02 s. Two cases differ in more than time:
  - a part before the snapshot part that is damaged under `#_on_corruption_# stop`: tsvz-rs, as §4.10 says,
    reads nothing after the damage, in that part or a later one, snapshot part included, and TSVZ.py, which
    begins at the snapshot part, reads it and does not see the damage; under `recover`, both read the same
    rows, and only tsvz-rs warns that the damaged part's recovered bytes are not verified;
  - a snapshot part that does not restate the parts before it, which a producer must not write: tsvz-rs reads
    every part and TSVZ.py only from the snapshot part.
  A `scrub` writes the marker and finds a part marked.
- Why: finding the part before the read took a look at the start of every part, apart from the read. In
  0.1.4's reviews that look cost a second decode of each part's start (most on bzip2 parts and on a snapshot
  part of one large member), and leaving bzip2 out of it made the answer on such a store depend on the codec.
- Revisit when: the next tsvz-rs version. The plan: the read judges a part from its first lines as it decodes
  them, one rule for every codec, and a look before the read only chooses where to begin, from each part's
  first page; the read of that part confirms it, or the store is read again from part 0. It must also choose,
  for the first case, between §4.10 and TSVZ.py's start (§19.10.3 says the result is the same, which does not
  hold there).
- Noted: 2026-11-16, in the 0.1.4 fix rounds; a deferral under the user's standing rule that deferrals are
  noted, not asked.

### A loose scrub and a one-column delete hold every line of the file in memory
- Kind: deferred
- What the operator sees: `tsvz scrub` of a loose file, and `delete`, `pop` and `popitem` in a loose file of
  one column, use memory that grows with the file's length, 3 to 11 times its size, also when most of its lines
  are old values of a few keys. 0.1.3's scrub needed memory for the live rows only, and its delete appended one
  line. Peak memory in MiB (0.1.3 in brackets), on files of lines `key%07d`, three cells of 16 hex digits and
  text (`k%08d` alone for the delete), tsvz-rs 0.1.4 as released:
  - scrub, 200,000 lines of 1,000 keys (11.4 MiB): 36 (15);
  - scrub, 800,000 lines of 200,000 keys (45.8 MiB): 176 (127);
  - scrub, 4,000,000 lines of unique keys (228.9 MiB): 1,373 (2,273), less than 0.1.3;
  - delete of one key, 5,000,000 one-column lines (47.7 MiB): 527 (16) and about 1 s, where 0.1.3 appended one
    line and took no time (TSVZ.py's own delete: 1,422 MiB, 7 s).
- Why: the rewrite keeps each line it keeps as the file has it, so it holds every line, each in its own buffer,
  until it decides which go.
- Revisit when: the next tsvz-rs version. The plan: rewrite in two passes. The first keeps a kind and a key number
  for each line, and each key's latest line; the second streams the file and writes the kept lines. Memory then
  follows the keys, as 0.1.3's did. A one-column delete streams the same way: one pass to see which lines go, one
  to write the rest.
- Noted: 2026-11-16, in the 0.1.4 fix rounds; a deferral under the user's standing rule that deferrals are
  noted, not asked.

### A loose delete reads a compressed file whole
- Kind: deferred
- What the operator sees: `delete`, `pop` and `popitem` in a compressed loose file of more than one column read
  the whole compressed file into memory and decode it up to its first keyed line. On 89 MiB of gzip (4,000,000
  rows, 228 MiB of text) that takes 0.1 s and 94 MiB in 1 MiB members, and 0.7 to 0.8 s and 348 MiB (the header
  in a member of its own) to 577 MiB (the header in the one member) as one member, where 0.1.3 took 0.00 s
  and 15 MiB (the times are from a busy host: 0.06 s and 0.5 s on a quiet one). A plain file is read only as far
  as its first keyed line.
- Why: the read decodes a compressed part as one slice, where a damaged member's bytes run to the next member's
  start, found by looking ahead; a separate streaming look at the first lines read past a damaged member into the
  next one (0.1.4's third review), so it was removed.
- Revisit when: the next tsvz-rs version. The plan: decode a part from the file a member at a time; the delete's
  read then stops as a plain file's does, with no second decoder.
- Noted: 2026-11-16, in the 0.1.4 fix rounds; a deferral under the user's standing rule that deferrals are
  noted, not asked.

### A scrub's look at a part's start can judge an odd part wrongly, and reads a long first line whole
- Kind: deferred
- What the operator sees: a multi-part `scrub` finds a part marked by its first lines. It can take a gzip part
  that begins with `#_snapshot_#` for no snapshot part: a stored (uncompressed) member that holds the gzip magic
  bytes `1f 8b 08` in its first 4 KiB. The look has only the start of the part, and takes the magic for the start
  of a next member after a cut one, where the read decodes the whole part as one member (the second review's
  probe: 1 of 800 random gzip parts, none of 800 each in xz, zstd and bzip2). The scrub then
  writes one more marked copy of the part, as 0.1.3's scrub always wrote one, and a later scrub finds that copy
  marked. A read is unaffected. A part whose first line is very long is also decoded to the end of that line
  before the look says "no": a first line of 2 MB in an 18.7 KB gzip member was read whole.
- Why: the look decodes a part's first page by a streaming decoder of its own, and finds where a member ends and
  where a line ends by decoding on, where the read decodes a part as one slice.
- Revisit when: the next tsvz-rs version. The plan: the part reader of the entry above, which decodes a member at
  a time, serves the look too, with one rule for where a member ends.
- Noted: 2026-11-16, in the 0.1.4 fix rounds; a deferral under the user's standing rule that deferrals are
  noted, not asked.
