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

### An append to a compressed loose file joins its unterminated last line
- Kind: deferred
- What the operator sees: a compressed loose file (`.tsv.gz` and the like) whose decoded text does not end with a
  newline (a file compressed from one saved without a final newline) has its last line joined by the next `set`,
  `append` or `delete`: the appended member's first record continues that line, so the record is not read, and
  the last row reads with its cells run on (`b<TAB>1` then `delete b` reads `b<TAB>1b`). TSVZ.py's writers do
  the same ("a compressed file's last line is not looked at"). A plain loose file gets a newline first, as
  TSVZ.py's does.
- Why: whether the decoded text ends with a newline is known only by decoding the file to its end, which an
  append does not do.
- Revisit when: 0.2.2. The plan: begin each member a writer appends to a compressed loose file that holds
  something with a newline: an empty line is read as nothing by both tools, so the cost is a byte per append,
  and the line before it is ended; or learn the end from the last member, found by the part reader.
- Noted: 2026-11-23, in the 0.2.1 build (the loose differential test); a deferral under the user's standing rule
  that deferrals are noted, not asked.

### A handler on a cifs (SMB) mount is unreachable for the command line, on older client kernels
- Kind: deferred
- What the operator sees: on a cifs mount from a Linux client older than 7.0 (measured: 6.1 and RHEL 8's 4.18;
  a 7.0 client is not affected, nor a share mounted with `nobrl`, whose locks then exclude only the processes of
  one client), while `tsvz serve`
  runs on a store there, every other routed `tsvz` command on that store (`read`, `get`, `has`, `len`, `keys`,
  `set`, `append`, `delete`, `clear`, `scrub`, `pop`, `popitem`, `setdefault`) says `ignoring the pointer file:
  PATH: Permission denied (os error 13); using the files directly` and works on the files, as `--x-direct` does.
  `tsvz stop` fails with the same `Permission denied`, so the handler is stopped by a signal (or by
  `--x-idle-timeout`). A second `tsvz serve` is refused with `cannot take the pointer file: ... Permission
  denied`. TSVZ.py 5.18's command line fails every command there (`[Errno 13] Permission denied`, exit 1): its
  lookup reads the pointer file outside the `try` that catches the open. On those clients a direct read of a store
  that another process is appending to can fail too (`Permission denied`, the read incomplete), whichever tool
  appends: both tools' append locks cover bytes a read reads.
- Why: the handler holds its pointer file with `flock`, as TSVZ.py's does, and cifs on those kernels turns it into
  a mandatory SMB byte-range lock over the file, so another process's read of it fails with EACCES; the append's
  lock (whole-file on Linux in tsvz-rs, byte 1 in TSVZ.py, the layout of §18.6) is made mandatory the same way.
  Measured on Samba 4.17 with 7.0, 6.1 and 4.18 clients (2026-11-23).
- Revisit when: 0.2.2, together with TSVZ.py. The plan, measured on all three kernels: a handler holds a write
  lock (an open file description lock) on one byte far past the pointer file's content (offset 2^62), where no
  reader reads, and treats EACCES and EAGAIN as busy; readers then read the pointer, and a second handler is
  refused. Both tools must change together: on a local file system `flock` and such a lock do not exclude each
  other, so a handler of one tool that took the new lock would not exclude a handler of the other that takes
  `flock`. TSVZ.py's lookup must also catch an error of the read. The append lock's range is a spec matter (§18.6)
  and goes to the TSVZ session; with a reachable handler the command line reads through its socket and never meets
  it.
- Noted: 2026-11-17, in the 0.2.0 build; measured and planned 2026-11-23, in the 0.2.1 build; a deferral under the
  user's standing rule that deferrals are noted, not asked.

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

### Reads of a loose file take more CPU than 0.2.0's, and pad every line
- Kind: deferred
- What the operator sees: reading a loose file (`.tsv .csv .nsv .psv`) costs more CPU than in 0.2.0: on a 120 MB
  `.tsv` of 2,000,000 rows of four cells, `get` of one key takes 0.21 s (0.2.0: 0.10 s), `len` 1.27 s (1.01 s), `keys`
  1.18 s (1.03 s) and `read` 1.64 s (1.28 s); memory is the same, and a strict store is not affected. A file whose
  first keyed line is very wide (a second opinion's reading, not measured: 10,000 tabs, then a million rows) costs a
  step per
  padded cell on every line, and a whole read holds every row padded to that width (8 bytes a cell), where 0.2.0 held
  each row's own cells and padded them when it printed them.
- Why: a loose line is read by TSVZ.py's line rule, which splits and decodes every cell of every line, and pads it to
  the file's column count, before the read asks whether it wants that key.
- Revisit when: 0.2.2. The plan: decode a line's key, count its cells and test them for a value without building them;
  decode and pad the cells only for a `#_defaults_#` line or a key the read wants (a row keeps its own cells and is
  padded when it is read).
- Noted: 2026-11-23, in the 0.2.1 final review; a deferral under the user's standing rule that deferrals are noted,
  not asked.

### A head read can hold what one page of compressed input decodes to
- Kind: deferred
- What the operator sees: a loose `delete` in a compressed file (a read that stops at the file's first keyed line)
  holds about 1 MiB of a member and, by the formats' best ratios, as much as one 64 KiB page of input decodes to
  before the next check: up to 64 MiB for deflate (1032 to one) and 2 GiB for zstd (a 128 KiB run-length block in 4
  bytes). Only crafted input reaches that (from the formats' ratios, not measured); files compressed by tsvz-rs or
  TSVZ.py stay near 1 MiB.
- Why: the decoder takes a whole page of input per call, and the part reader checks the output's length after the
  call.
- Revisit when: 0.2.2. The plan: give the decoder a bounded output buffer per call (the part reader then tells a stop
  at that cap from a stall), and say the real bound in `codec::read_items`'s comment.
- Noted: 2026-11-23, in the 0.2.1 final review; a deferral under the user's standing rule that deferrals are noted,
  not asked.

### A request's own `#_write_ack_#` marker does not make that request's writes durable
- Kind: deferred
- What the operator sees: a bulk `set -` through a handler that carries `#_write_ack_#<TAB>disk` and then `k<TAB>v` is
  answered at once, and `k<TAB>v` is not synced, though the files now say `disk`: after a power loss it can be gone,
  where §21.9.2 says a write made while `disk` is in force is acknowledged after its sync. TSVZ.py decides the same
  way.
- Why: durability is decided before the request's own markers are queued; whether a request's markers apply to the
  rest of that request is a question of the specification (§21.9.2) that both tools answer alike.
- Revisit when: 0.2.2. The plan: settle the rule with the TSVZ session (a request's markers apply from the line after
  them), put it in §21.9.2 and a conformance case, then queue the request's markers before deciding its durability in
  both tools.
- Noted: 2026-11-23, in the 0.2.1 final review; a deferral under the user's standing rule that deferrals are noted,
  not asked.

### `tsvz stop` can wait 30 s and fail where the old stop answered at once
- Kind: deferred
- What the operator sees: two cases (a second opinion's reading of the code, not reproduced). (1) The handler closes a
  connection unanswered for a reason of its own (no descriptor left, no thread to start): `tsvz stop` takes that for
  "stopping already", waits 30 s and fails with `has not stopped within 30 s` while the handler serves (0.2.0 failed
  at once). (2) A handler killed after it replied to a stop and before it removed its pointer file leaves the file,
  which still names it: `tsvz stop` waits 30 s and fails (0.2.0 gave 0 after 10 s). (3) The same stale pointer after
  a routed `pop` or `popitem`: a handler that dies after answering the pop and before the command prints the row (the
  put-back path, `cli::route`) leaves its pointer, so the command waits 30 s for it to go, leaves the files alone and
  prints the row on standard error (0.2.0 printed the row at once and never put it back).
- Why: the stop, and the put-back after a routed pop, tell a handler that is gone from one that is not by the
  pointer file's name alone.
- Revisit when: 0.2.2. The plan: also try the pointer file's `flock`, which the system releases when the handler's
  process ends (gone when it can be taken), and after a close with no answer ask once more after a short wait: a
  handler that answers is serving, and the stop says so at once. The put-back asks the same question
  (`pointer::wait_gone`), so a dead handler's row goes back at once.
- Noted: 2026-11-23, in the 0.2.1 final review; a deferral under the user's standing rule that deferrals are noted,
  not asked.

### A routed `pop` or `popitem` whose answer is lost in transit loses its row
- Kind: deferred
- What the operator sees: the handler deletes the row before its answer is written to the client: a client that times
  out or hangs up in between (a killed command, a broken connection) has neither printed the row nor put it back, and
  the error (`lost the handler`) names no row. 0.2.0 does the same; 0.2.1 puts back a row the command could not print,
  not one it never received (a second opinion's finding, by reading).
- Why: the pop is one step in the handler: take the row, queue its deletion, answer.
- Revisit when: 0.2.2. The plan: a pop in two steps: the handler holds the row hidden from other requests until its
  answer is written, then deletes it, and gives it back when the write fails; or accept it as a known limitation, with
  the user's OK.
- Noted: 2026-11-23, in the 0.2.1 final review; a deferral under the user's standing rule that deferrals are noted,
  not asked.

### A stop that cannot write or sync its queue still ends cleanly
- Kind: deferred
- What the operator sees: a second opinion's reading of the code, not reproduced: `stop` flushes the queue and replies
  `#0` though a batch of plain (not `--x-written`) writes failed, so a write the handler had acknowledged is lost; a
  failed flush replies `#1` and the handler stops anyway; a failed sync at exit is only logged and the handler ends
  with status 0; the close has a narrow window between its last sync and the pointer file's removal. The command
  line's own writes are answered once written, so only a raw client sees the first.
- Why: the stop path does not carry a failure of the writer's last batch into its answer, the handler's exit status or
  its log.
- Revisit when: 0.2.2. The plan: the writer remembers the first failed write or sync; a stop answers `#1` with it,
  `serve` ends with status 1 and logs it as an error, and the sync at close is made before the pointer file goes.
- Noted: 2026-11-23, in the 0.2.1 final review; a deferral under the user's standing rule that deferrals are noted,
  not asked.

### `serve` leaves SIGINT, SIGTERM and SIGHUP blocked once it has returned
- Kind: deferred
- What the operator sees: a program that embeds `handler::server::serve` (a hidden function) and goes on after a stop
  has the three signals blocked in its calling thread, and a signal thread waiting in `sigwait` for the life of the
  process. `tsvz serve` itself exits.
- Why: `serve` blocks the signals for the thread it started in and never restores the mask.
- Revisit when: 0.2.2. The plan: restore the mask and end the signal thread when `serve` returns.
- Noted: 2026-11-23, in the 0.2.1 final review; a deferral under the user's standing rule that deferrals are noted,
  not asked.

### The stop's gone check compares the process id alone
- Kind: deferred
- What the operator sees: a pointer file of another handler that has the process id of the one `tsvz stop` stopped (a
  pid that was reused) is that handler to `tsvz stop`, which waits for it to go (a second opinion's reading, not
  reproduced).
- Why: the gone check compares the pid alone.
- Revisit when: 0.2.2. The plan: compare the pointer's address too.
- Noted: 2026-11-23, in the 0.2.1 final review; a deferral under the user's standing rule that deferrals are noted,
  not asked.

### A scrub of a loose file can change a row a strict TSVZ.py reader reads
- Kind: deferred
- What the operator sees: a loose `scrub` moves a key's last line into its first line's place even when it has fewer
  cells (`K 1 2` then `K 9` becomes `K 9`): both tools' command lines read `K` alike before and after (`[K, 9, ]`),
  but TSVZ.py's strict read of the library (`readTSV(strict=True)`, which drops a narrower line) read `K` as `[K, 1,
  2]` before and finds no `K` after. TSVZ.py's scrub docstring promises that every reader reads the file as before.
  0.2.0 does the same.
- Why: the scrub keeps what a reader of the command line reads, and takes no strict reader's reading into account.
- Revisit when: 0.2.2. The plan: decide with the TSVZ session whether a scrub must keep what a strict reader takes: if
  so, move a key's last line into its first line's place only where it has at least as many cells; if not, record it
  as a known limitation (the user's OK).
- Noted: 2026-11-23, in the 0.2.1 final review; a deferral under the user's standing rule that deferrals are noted,
  not asked.

### Bulk input to a loose file is read by its own rule, not the loose line rule
- Kind: deferred
- What the operator sees: `tsvz set f.tsv -` (and `append`, `delete -`) with a loose-format payload drops a
  `#_defaults_#` line as a comment instead of writing it, where a reader reads it as the defaults. 0.2.0 does the
  same. (0.2.1 drops the NULs at a line's start, as a reader does.)
- Why: `ops::parse_records` tests the raw first field for a comment, and a `#_defaults_#` line is one.
- Revisit when: 0.2.2. The plan: give bulk input of a loose format to `loose::parse`, the one rule a reader uses (a
  `#_defaults_#` line is a record, a comment is skipped).
- Noted: 2026-11-23, in the 0.2.1 final review; a deferral under the user's standing rule that deferrals are noted,
  not asked.

### A loose `clear` writes no blank line 1 before the defaults it keeps
- Kind: deferred
- What the operator sees: when line 1 of a loose file is neither a comment nor empty and `clear` keeps `#_defaults_#`
  lines (it does, by the user's pick of 2026-11-14), the cleared file begins with a defaults line; TSVZ.py's `clear`
  writes an empty line 1 (and keeps no defaults), so a TSVZ.py program that reads the file with a header takes the
  defaults line for its header (a second opinion's reading of TSVZ.py, not run). Both tools' command lines read the
  file alike.
- Why: `ClearPlan` keeps line 1 only when it is a comment or empty.
- Revisit when: 0.2.2. The plan: write the blank line 1 before the defaults it keeps (the tests in `loose.rs` that
  encode its absence change), or record the departure in docs/silent-choices.md.
- Noted: 2026-11-23, in the 0.2.1 final review; a deferral under the user's standing rule that deferrals are noted,
  not asked.

### `scrub` and `clear` refuse a loose file whose name ends in a number
- Kind: deferred
- What the operator sees: `tsvz scrub app.tsv.1` or `clear` of a loose file named `<x>.tsv.<number>` says it is one
  part of a multi-part store and refuses; `delete` and `list` treat it as one file. 0.2.0 does the same.
- Why: the name is read as a numbered part (§17.2), which a loose file has none of.
- Revisit when: 0.2.2. The plan: a loose path's numeric suffix names a file, not a part, for `scrub` and `clear` as
  for the rest (`name.rs`); check §17.2 and §5.2 with the TSVZ session first.
- Noted: 2026-11-23, in the 0.2.1 final review; a deferral under the user's standing rule that deferrals are noted,
  not asked.

### A loose `delete` names a key with a newline or a delimiter at its end by the key without it
- Kind: deferred
- What the operator sees: `tsvz delete f.tsv $'a\n'` writes the tombstone for `a`, and deletes it, though `a` and
  `a<LF>` are two keys. 0.2.0 does the same. The
  same in `setdefault`: `setdefault f.tsv '#_defaults_# ' x y` (a blank at the key's end) passes its marker-key
  check, which looks at the key untrimmed, and writes a line that a reader takes for the defaults line and prints as
  a row, exit 0 (`setdefault f.tsv '#_defaults_#' x` is refused, exit 2; 0.2.0 wrote the same line but applied no
  defaults).
- Why: the key is trimmed of whitespace before it is encoded, where a reader trims the cell as the file has it.
- Revisit when: 0.2.2. The plan: encode the key, then trim it (`loose::sanitize` and `py_space`), as a reader names
  it, before `delete` makes its names and `setdefault` its marker-key check.
- Noted: 2026-11-23, in the 0.2.1 final review; a deferral under the user's standing rule that deferrals are noted,
  not asked.

### A loose delete that loses a race backs the file up again
- Kind: deferred
- What the operator sees: a loose `delete` that rewrites a damaged file, and whose rewrite is overtaken by another
  writer's append, makes another `.damaged-<time>` copy on its next attempt and says the damage twice. 0.2.0 does the
  same.
- Why: the backup is made before each attempt's rewrite.
- Revisit when: 0.2.2. The plan: make the backup once, when an attempt is about to commit its rewrite.
- Noted: 2026-11-23, in the 0.2.1 final review; a deferral under the user's standing rule that deferrals are noted,
  not asked.

### A loose delete's guard does not cover a first keyed line appended after the file was read
- Kind: deferred
- What the operator sees: a second opinion's reading of the code, not reproduced. An empty (or comment-only) loose
  file; `tsvz delete f.tsv k` takes the file's size and last bytes (nothing); another process appends `a<TAB>1`; the
  delete's head read sees two columns; a `clear` (which rewrites the file in place, the same inode) empties it again;
  under the lock the file is as it was first seen (same inode, no shorter, the same empty end), so `k` is appended and
  `f.tsv` now holds one row, `k`. 0.2.0 appended after any clear, a wider window.
- Why: the decision (the column count) rests on the bytes up to the first keyed line, which here lie past the size the
  guard covers.
- Revisit when: 0.2.2. The plan: append only when the head read's first keyed line ends within the size seen (else
  read again), or read the head again under the lock.
- Noted: 2026-11-23, in the 0.2.1 re-review of the final-review fix round (E); a deferral under the user's standing
  rule that deferrals are noted, not asked.

### The header of a loose file keeps the NULs at the end of an unterminated first line comment
- Kind: deferred
- What the operator sees: `header()` of a loose file whose line 1 is a comment with no newline and ends in NULs (a
  torn write) returns the comment with those NULs; a read of the file strips them as TSVZ.py does. A trivial
  difference in the library's `header()`; the command line prints no header.
- Why: `header()` trims the NULs at the start of line 1 and not the ones at the end of an unterminated one.
- Revisit when: 0.2.2. The plan: one more trim of the NULs at the end when line 1 is unterminated, as `loose::parse`
  does.
- Noted: 2026-11-23, in the 0.2.1 re-review of the final-review fix round (F5); a deferral under the user's standing
  rule that deferrals are noted, not asked.
