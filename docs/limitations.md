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
  - Readers would take READ: byte 0 shared on Linux, through the gate (byte 2 shared, let go of once byte 0 is held);
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
  that file. A `tsvz serve` that takes a write while another writer changes the store (its follow can close the
  store's descriptor of the part while its writer thread appends a batch), or a program that reads a store in one
  thread while it appends in another, can then lose the lock for a moment, and a TSVZ.py writer appending at that
  moment can interleave with it, so a record could be lost. A single command (`tsvz set`, `scrub`) opens no other
  descriptor of a file it holds locked.
- Why: 0.1.x kept every descriptor of a locked file open until the lock was released (`tsvz::lock::Handle`, a
  process-wide table), because macOS took classic locks then. Once macOS takes `flock`, the table would guard only
  the cases above, for 278 code and 240 test lines (the net the removal took out: `src/` above each file's
  `mod tests {` counts as code, the rest of `src/` and `tests/` as tests; the question's estimate had been about 350
  and 225), a CI step and four fixes so far; 0.2.0 removed it.
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
  `setdefault`) says `ignoring the pointer file: PATH: Permission denied (os error 13); using the files directly` and
  works on the files, as `--x-direct` does. `verify` and `parts` never look for a handler and work on the files.
  `tsvz stop` fails with the same `Permission denied`, so the handler is stopped by a signal (or by
  `--x-idle-timeout`). A second `tsvz serve` is refused (`cannot take the pointer file: ... Permission denied`, or
  `already served by pid ? on host ?`, by the error cifs gives for the conflicting lock, which was not measured).
  `tsvz::handler::Client::connect` finds no handler either. A client given the socket's address itself still reaches
  the handler. Measured on cifs: `get` beside an
  idle handler; the other commands were run, as a user other than root, with the pointer file's mode set to 000, which
  gives the same errno from `open`, so the message differs (`PATH is not a regular file (Permission denied (os
  error 13))`).
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

### Overlapping `tsvz stop`s: the loser names a dead handler, and a stop can exit 0 with the handler still running
- Kind: deferred
- What the operator sees: when two `tsvz stop STORE` overlap (or the second comes in the handler's finish window, or
  just after its listener closed), the first exits 0 and the handler stops; the other exits 1 with `the handler does
  not answer (... Connection refused ...)` or `(... No such file or directory ...)`, or `the handler went away
  (Connection reset by peer)`, where it could say the handler is stopping. In 150 overlapping pairs on 0.2.0, 25
  losers said one of these; 0.1.4 said `stop failed: the handler is stopping` in 7 of 150 and `does not answer` in 16.
  A stop that comes after the first has finished says `no handler is running` and exits 1, as designed. Also, a
  `tsvz stop` waits up to 10 s for the handler to be gone and then exits 0 whether or not it is, and its wait for the
  handler's reply has no limit. From reading; a handler that takes longer than 10 s to close was not reproduced.
  0.1.4 does the same.
- Why: `server::stop` turns a failed connect, write or read into `does not answer` or `went away` without asking
  whether the handler is already stopping, and ends its wait with `Ok(())`; the 10 s has no reason written next to it.
- Revisit when: 0.2.1. The plan: a stop that loses the race joins the wait for the handler to be gone, and every stop
  exits by whether the handler is gone; the 10 s gets a reason from a measure of how long a close takes (the finish
  window is at most 5 s, then the writer's queue and its syncs), and the reply gets a bound.
- Noted: 2026-11-17, in the 0.2.0 re-review; a deferral under the user's standing rule that deferrals are noted, not
  asked.

### The handler can end with SIGSEGV under a flood of connections on a loaded host
- Kind: deferred
- What the operator sees: `tsvz serve` ends by SIGSEGV (exit 139, a core where core dumps are on)
  while a client makes and closes connections in a tight loop on a heavily loaded host: its clients
  then find it gone (a write exits 1, a read uses the files), and writes it had acknowledged but not
  yet written are lost. Seen in 5 of 4,000 runs of the handler test binary, eight copies at once
  (`a_child_spawned_while_a_client_connects_never_inherits_its_connection`; debug builds, Rust
  1.100, glibc 2.41, Linux 7.0); a flood alone (16 processes, up to 990,000 connections in 10 s, 16
  rounds, with and without a 120-task limit) did not reproduce it. 0.2.0 is the same.
- Why: not diagnosed. The four cores kept (of five crashes) all stop in `pthread_detach`, called by
  the drop of the `JoinHandle` that `std::thread::Builder::spawn` had just returned in
  `server::start_connection`; the one read that far faults at `pthread_detach+28`, reading the new
  thread's descriptor, whose memory is no longer mapped: the thread's stack was freed before its
  handle was detached. Whether glibc, std or the handler is at fault is not known.
- Revisit when: 0.2.2. The plan: reproduce with the test loop (eight lanes of the handler test
  binary) on a release build and on Rust 1.85; read the cores with glibc's debug symbols; if it is
  glibc's or std's, draft an upstream report and work around it in one place (a connection thread
  that is joined rather than detached, say); else fix it.
- Noted: 2026-11-23, in the 0.2.1 build (h021); a deferral under the user's standing rule that
  deferrals are noted, not asked.

