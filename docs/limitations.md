# Limitations

Approved known limitations and deferrals. Reviewers: read this first, and don't raise these again.

### Rust readers take no lock, so they can see a TSVZ 5.3 rewrite part way
- Kind: deferred
- What the operator sees: TSVZ.py 5.3 readers of a `.tsv`-family file take a shared lock while they read (byte 0 of
  an open file description lock on Linux, `flock` `LOCK_SH` on macOS), so a rewrite under the file lock is waited
  out. tsvz-rs readers take none: like TSVZ.py 5.2, they can read another process's
  in-place rewrite part way, and a delete made in that moment can be lost. Compatibility needs nothing on Linux:
  tsvz-rs's whole-file write lock conflicts with every 5.3 lock (its appends now also wait for Python readers,
  since byte 0 is inside its whole-file lock: slower, not wrong). On macOS tsvz-rs takes `flock` as 5.3 does (since
  0.2.0), which also excludes 5.2's `lockf` (TSVZ's macOS CI job checked it, 2026-11-04), so it holds there too.
- Why: tsvz-rs follows TSVZ.py; the change is a lock-layer change of its own:
  - Readers take READ: byte 0 shared on Linux, through the gate (byte 2 shared, let go of once byte 0 is held);
    `flock` `LOCK_SH` on macOS, except on a file this thread holds the write lock on (it would wait for itself).
  - `maint.rs` rewrites would take byte 1, the gate (byte 2), then the whole file; appends would move to byte 1, so
    they do not wait for Python readers.
  - Tests: extend `tests/interop.rs` with "a Python READ waits for a Rust rewrite" and "a Rust reader waits for a
    Python rewrite".
- Revisit when: someone shares a file between tsvz-rs and another writer that rewrites it.
- Approved: 2026-11-04, the user chose "Deferred in tsvz-rs (Recommended)".

### Where neither file lock exists, a multi-threaded process can lose its write lock for a moment
- Kind: known limitation
- What the operator sees: tsvz-rs takes TSVZ.py's write lock, an open file description lock on Linux and `flock` on
  macOS. Where the kernel or file system refuses it (Linux before 3.15 without the backport, which is CentOS 7.0 to
  7.4, outside the targets, answering `EINVAL`; or a mount that refuses `flock` on macOS, answering `ENOTSUP`,
  `EOPNOTSUPP` or `EINVAL`; any other error fails the write, as in TSVZ.py), the lock is a classic POSIX lock, as
  TSVZ.py's own fallback (`lockf`) is, and the process loses it when any of its threads closes another descriptor of
  that file. A `tsvz serve` that answers a read while its writer appends, or a program that reads a store in one thread
  while it appends in another, can then lose the lock for a moment, and a TSVZ.py writer appending at that moment
  can interleave with it, so a record could be lost. A single command (`tsvz set`, `scrub`) opens no other
  descriptor of a file it holds locked.
- Why: 0.1.x kept every descriptor of a locked file open until the lock was released (`tsvz::lock::Handle`, a
  process-wide table), because macOS took classic locks then. Once macOS takes `flock`, the table would guard only
  the cases above, for 278 code and 240 test lines (the measured removal; the question said about 350 and 225), a
  CI step and four fixes so far; 0.2.0 removed it.
- Revisit when: an operator shares a store with another writer on a system without either lock.
- Approved: 2026-11-17, the user's pick (relayed by chief): "Switch, accept it (Recommended)".

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
- Revisit when: 0.2.1. The plan: read a loose file's tail as a line, and write a
  newline before an append instead of cutting it, as TSVZ.py does.
- Noted: 2026-11-15, in the 0.1.4 build; a deferral under the user's standing rule that deferrals
  are noted, not asked.

### Loose reads still differ from TSVZ.py's in six ways
- Kind: deferred
- What the operator sees: in a loose file, `tsvz read`, `get` and `keys` differ from TSVZ.py's command line in
  six ways (one shape is new in 0.1.4: a line of only whitespace alone in a one-column file is a row, as every
  key alone there is since 0.1.4, where 0.1.3 read every key alone as a deletion):
  - the values of a `#_defaults_#` line are not filled into the empty cells of the rows after it (tsvz-rs reads
    the line as a comment);
  - a `#_defaults_#` line with no value, bare or with blank cells, in a one-column file is a row to TSVZ.py,
    and no row to tsvz-rs;
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
- Revisit when: 0.2.1. The plan: port TSVZ.py's line rule (`_processLine`, with `_unsanitize`
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
- Revisit when: 0.2.1. The plan: append only while the file still has the stamp
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
- Why: finding the part to begin at takes a look at the start of every part, apart from the read. 0.1.4 tried
  that look in the read and removed it from the read (a scrub still uses it to find a marked part): it cost a second decode of each part's start (most on bzip2 parts and on a
  snapshot part of one large member), and leaving bzip2 out of it made the answer on such a store depend on
  the codec.
- Revisit when: 0.2.1. The plan: the read judges a part from its first lines as it decodes
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
  line. Peak memory in MiB (0.1.3 in brackets), release builds of the 0.1.4 branch (2026-11-16), on files of
  60-byte lines: `key%07d`, two cells of 16 hex digits and `some text here` (`k%08d` alone for the delete):
  - scrub, 200,000 lines of 1,000 keys drawn at random (11.4 MiB): 36 (15);
  - scrub, 800,000 lines of 200,000 keys drawn at random (45.8 MiB): 176 (127); with the keys in round-robin
    order, 163 (152);
  - scrub, 4,000,000 lines of unique keys (228.9 MiB): 1,373 (2,273), less than 0.1.3;
  - delete of one key, 5,000,000 one-column lines (47.7 MiB): 527 (16) and about 1 s, where 0.1.3 appended one
    line and took no time (TSVZ.py's own delete: 1,422 MiB, 7 s).
- Why: the rewrite keeps each line it keeps as the file has it, so it holds every line, each in its own buffer,
  until it decides which go.
- Revisit when: 0.2.1. The plan: rewrite in two passes. The first keeps a kind and a key number
  for each line, and each key's latest line; the second streams the file and writes the kept lines. Memory then
  follows the keys, as 0.1.3's did. A one-column delete streams the same way: one pass to see which lines go, one
  to write the rest.
- Noted: 2026-11-16, in the 0.1.4 fix rounds; a deferral under the user's standing rule that deferrals are
  noted, not asked.

### A loose delete reads a compressed file whole
- Kind: deferred
- What the operator sees: `delete` in a compressed loose file reads the whole compressed
  file into memory and decode it a member at a time, up to the end of the member that holds its first keyed
  line. A file of more than one column is then done (it appends a key alone), and says only the damage it met
  on the way; one of one column is read whole again, for the rewrite. On 4,000,000 rows of the layout above
  (228 MiB of text, 89 MiB of gzip at level 6) the delete takes 0.1 s and 94 MiB in 1 MiB members, and 0.7 to
  0.8 s and 348 MiB (the header in a member of its own) to 577 MiB (the header in the one member) as one
  member, where 0.1.3 took 0.00 s and 15 MiB (the times are from a busy host: 0.06 s and 0.5 s on a quiet
  one). `pop` and `popitem` first read the whole file, as 0.1.3 did, and then do the same. A `delete` of a plain
  file of more than one column reads only as far as its first keyed line.
- Why: the read decodes a compressed part as one slice, where a damaged member's bytes run to the next member's
  start, found by looking ahead; a separate streaming look at the first lines read past a damaged member into the
  next one (0.1.4's third review), so it was removed.
- Revisit when: 0.2.1. The plan: decode a part from the file a member at a time; the delete's
  read then stops as a plain file's does, with no second decoder.
- Noted: 2026-11-16, in the 0.1.4 fix rounds; a deferral under the user's standing rule that deferrals are
  noted, not asked.

### Three edges of how a loose file names its keys, left to the port of TSVZ.py's line rule
- Kind: deferred
- What the operator sees: three narrow cases where tsvz-rs 0.1.4 names a loose line's key differently from
  TSVZ.py. None is worse than 0.1.3 or loses data.
  - A one-column `delete` that leaves only `#_defaults_#` lines: `c`, `#_defaults_#<TAB>P`,
    `#_defaults_#<TAB>`, then `delete c`. tsvz-rs writes a bare `#_defaults_#` line in front, where TSVZ.py's
    own delete writes none. TSVZ.py then lists `#_defaults_#` as a key of tsvz-rs's file (a bare line at one
    column is a row to it) and no key of its own; tsvz-rs lists none of either. 0.1.3 appended `c`, and TSVZ.py
    listed `c` and `#_defaults_#`.
  - A loose file followed through `Store::refresh` (the library: no command follows a loose file) whose line 1
    was not complete when the store was opened: `<BOM>a<TAB>x` with no newline, then a newline appended. The
    followed store lists `a`, a store opened afterwards `<BOM>a`, the key TSVZ.py reads. 0.1.3 listed `a` both
    ways.
  - A `.nsv` file whose `#_defaults_#` line begins with a NUL: `0`, then `<NUL>#_defaults_#<NUL><NUL>`, then
    `delete 0`. The two name the file's keys differently before the delete (TSVZ.py lists `0` and
    `#_defaults_#`, tsvz-rs `0` only) and write the same bytes after it: tsvz-rs gives the file no stand-in
    key, so its width goes from one cell to three and the line becomes a reset, as TSVZ.py's own delete does.
    The same lines without the first NUL get the bare stand-in of the first case from tsvz-rs, and none from
    TSVZ.py.
- Why: tsvz-rs names a loose line's key in several places, each by a rule of its own (the rewrite's stand-in
  key, the line kinds of a scrub, the follow state's byte-order mark), where TSVZ.py has one function; these
  cases fall between them.
- Revisit when: 0.2.1. The plan: the port of `_processLine` that "Loose reads still differ
  from TSVZ.py's in six ways" plans, as one function for a line's key and kind in the read, the rewrite and the
  follow state, with TSVZ.py as the oracle; these cases go into its tests.
- Noted: 2026-11-16, in the 0.1.4 re-review; a deferral under the user's standing rule that deferrals are
  noted, not asked.

### A handler on a cifs (SMB) mount is unreachable for the command line
- Kind: deferred
- What the operator sees: while `tsvz serve` runs on a store on a cifs mount, every other routed `tsvz` command on
  that store (`read`, `get`, `has`, `len`, `keys`, `set`, `append`, `delete`, `clear`, `scrub`, `pop`, `popitem`,
  `setdefault`) says `ignoring the pointer file ...: Permission denied (os error 13); using the files directly` and
  works on the files, as `--x-direct` does. `verify` and `parts` never look for a handler and work on the files.
  `tsvz stop` fails with the same `Permission denied`, so the handler has to be stopped by a signal. A second
  `tsvz serve` is refused (the message depends on the error cifs gives for the conflicting lock, which was not
  measured). `tsvz::handler::Client::connect` finds no handler either. A client given the socket's address itself
  still reaches the handler. Measured: `get` beside an idle handler.
- Why: the handler holds its pointer file with `flock`, as TSVZ.py's does, and cifs turns that into a mandatory SMB
  byte-range lock, so another process's read of the file fails with EACCES. Found by the 0.1.4 measurements on a
  loopback SMB share (2026-11-17).
- Revisit when: 0.2.1. The plan: first, check TSVZ.py 5.18 from its code (a first look: its handler takes the same
  `flock`, and its lookup reads the file outside the `try` that catches only the open, so its command line may fail
  there rather than use the files; not run); then confirm both tools on a real cifs mount (VM 901, a test of its
  own); then fix it with TSVZ.py for both, such as by holding the lock where no reader of the pointer reads (another
  file, or a byte range past its content). The test also reads in another process while the handler appends: the
  append's whole-file lock (0.1.4 took the same) may be made mandatory on cifs too and deny that read with EACCES,
  and then "works on the files" does not hold for a store being written.
- Noted: 2026-11-17, in the 0.2.0 build; a deferral under the user's standing rule that deferrals are noted, not
  asked.

### A command that meets a handler just as it removes its pointer file can warn about a hard link
- Kind: deferred
- What the operator sees: a command that opens the pointer file just before a stopping handler removes it says
  `ignoring the pointer file ...: has another name (a hard link), so its owner may not have put it there; using the
  files directly`, then works on the files and exits 0. Only the text is wrong. Seen in 3 of about 48,000 `get`
  commands in a stress run that stopped a handler under 24 clients, with 0.1.4 and with 0.2.0 alike.
- Why: `lookup` refuses any link count but 1 (anyone may link a file they can see), and a file removed after it was
  opened has link count 0.
- Revisit when: 0.2.1. The plan: `lookup` treats link count 0 as no handler, without a warning.
- Noted: 2026-11-17, in the 0.2.0 final review; a deferral under the user's standing rule that deferrals are noted,
  not asked.

### A scrub or clear can rewrite a part that was rotated away between its open and its lock
- Kind: deferred
- What the operator sees: when the active part is renamed away (a rotation to `.rotated`, which the part listing
  skips) in the moment between a `scrub`, `clear` or `delete` opening it by path and taking its lock, the rewrite
  goes into the renamed file, and the command reports success while the active part still holds the rows it meant to
  change. From reading; the window was not reproduced.
- Why: `maint::rewrite` opens the part by path and locks it, and compares only the stamp of the file it opened with
  the one its caller read; `Writer::append_at` also checks, after its lock, that the path still names the file it
  holds. 0.1.4 is the same.
- Revisit when: 0.2.1. The plan: check `names` after the lock in `rewrite` as `append_at` does, so the rewrite is
  refused as a change by another writer and the scrub tries again; a test with a hook that renames the part between
  the open and the lock.
- Noted: 2026-11-17, in the 0.2.0 final review; a deferral under the user's standing rule that deferrals are noted,
  not asked.

### A row queued behind a failing `#_write_ack_# memory` batch is acknowledged without a sync
- Kind: deferred
- What the operator sees: a client sends `#_write_ack_# memory` to a store whose file says `disk`, and its batch
  fails to write (a file size limit, a full disk, an I/O error). A plain write another client sent before that
  failure was seen is acknowledged before it is written, and written without a sync, while the file still says
  `disk` (§21.9): a crash before the system writes it back loses a row the handler promised to sync. Rows sent after
  the failure was seen are synced again. Checked by running (a file size limit makes the marker's batch fail); 0.1.4
  does the same.
- Why: `Engine::submit` takes the acknowledgement mode and the member size from the markers still queued, which the
  failed batch never clears; the engine drops them only when it next follows the files.
- Revisit when: 0.2.1. The plan: a queued `memory` does not relax a `disk` the store has already written, until the
  store has followed the file; the cost is latency for rows sent in that window.
- Noted: 2026-11-17, in the 0.2.0 final review; a deferral under the user's standing rule that deferrals are noted,
  not asked.

### A pop that is still waiting when a stop's five seconds end loses its row
- Kind: deferred
- What the operator sees: a `pop` or `popitem` through a handler whose batch is waiting for a sync that outlasts the
  five seconds a stop allows (a slow disk, a flood of other writes) has queued its tombstone, which the stop writes
  before it ends, but its connection is cut before the row reaches the client: the row is gone from the store, the
  command exits 1 with `may or may not have been applied`, and nobody has the row. From reading; to be reproduced
  first. 0.1.4 drains the same way.
- Why: `finish_connections` gives requests already received `FINISH` (5 s) and then ends every connection still open;
  a pop is the one request whose answer is the data.
- Revisit when: 0.2.1. The plan: reproduce with a stalled sync; then wait without a limit for a request whose batch
  is being written, keeping the five seconds for a client that does not read its answer.
- Noted: 2026-11-17, in the 0.2.0 final review; a deferral under the user's standing rule that deferrals are noted,
  not asked.

### A pop on the files while a stopping handler has one in flight can take the same row
- Kind: deferred
- What the operator sees: once a stopping handler has closed its listener, a command that connects is refused and
  works on the files. A `pop` there can read a row as live that a `pop` already in flight in the handler took, whose
  tombstone the handler has not yet written, and take it too: two clients get the same row. The window lasts until
  that batch is written, longer under a slow sync. From reading; to be reproduced first. 0.1.4 closes its listener
  at the same point.
- Why: the listener goes first and the pointer file stays until the queue is written, so a command in the window
  finds a pointer file with nothing behind it and does what it does when a handler is gone.
- Revisit when: 0.2.1. The plan: reproduce with a stalled batch; then hold the listener open until the queued
  tombstones are written, so that a command in the window is closed (a write exits 1) and does not run on the files.
- Noted: 2026-11-17, in the 0.2.0 final review; a deferral under the user's standing rule that deferrals are noted,
  not asked.
