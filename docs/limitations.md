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

### A command that meets a handler just as it removes its pointer file can warn about a hard link
- Kind: deferred
- What the operator sees: a command that opens the pointer file just before a stopping handler removes it says
  `ignoring the pointer file: PATH has another name (a hard link), so its owner may not have put it there; using the
  files directly`, then works on the files and exits 0. Only the text is wrong. `tsvz stop` does not fall back: it
  exits 1 with the same words (`PATH has another name (a hard link), so its owner may not have put it there`).
  Seen in 6 of about 141,000 `get` commands over three stress runs of 0.2.0 builds that stopped a handler under 24
  clients (Rust 1.85.1), and in none of about 51,000 in the run of 0.1.4; the code that refuses is the same in 0.1.4.
- Why: `lookup` refuses any link count but 1 (anyone may link a file they can see), and a file removed after it was
  opened has link count 0.
- Revisit when: 0.2.1. The plan: `lookup` treats link count 0 as no handler, without a warning (`tsvz stop` then
  says `no handler is running`, and exits 1 as it does for a handler that is not there).
- Noted: 2026-11-17, in the 0.2.0 final review; a deferral under the user's standing rule that deferrals are noted,
  not asked.

### A scrub or clear can rewrite a part that was rotated away between its open and its lock
- Kind: deferred
- What the operator sees: when the part a `scrub` or `clear` has opened (a scrub's target, the last of the parts
  before the active one, or the only part of a one-part store; `clear` rewrites the only part) is renamed away by
  another scrub's rotation (to `.rotated`, which the part listing skips) in the moment between its opening by path
  and taking its lock, the rewrite goes into the renamed file, and the command reports success while the store
  still holds the rows it meant to change. A loose file is never rotated. From reading; the window was not
  reproduced.
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
  fails to write (a full disk or an I/O error; also a file size limit, when the handler ignores SIGXFSZ, whose
  default action ends the process). A plain write another client sent before that failure was seen is acknowledged
  before it is written, and written without a sync, while the file still says `disk` (§21.9): a crash before the
  system writes it back loses a row the handler promised to sync. Rows sent after the failure was seen are synced
  again. Checked by running (a file size limit, with SIGXFSZ ignored, makes the marker's batch fail); 0.1.4 does the
  same.
- Why: `Engine::submit` takes the acknowledgement mode and the member size from the markers still queued, which the
  failed batch never clears; the engine drops them only when it next follows the files.
- Revisit when: 0.2.1. The plan: a queued `memory` does not relax a `disk` the store has already written, until the
  store has followed the file; the cost is latency for rows sent in that window.
- Noted: 2026-11-17, in the 0.2.0 final review; a deferral under the user's standing rule that deferrals are noted,
  not asked.

### A pop that is still waiting when a stop's five seconds end loses its row
- Kind: deferred
- What the operator sees: a stop by a signal (SIGINT, SIGTERM or SIGHUP), or a pop that arrives after a `tsvz stop`
  request's flush, while a `pop` or `popitem`'s batch is still being written when the five seconds the stop allows
  end (a slow disk, a flood of other writes): the pop has queued its tombstone, which the stop writes before it
  ends, but its connection is cut before the row reaches the client. The row is gone from the store, the command
  exits 1 with `may or may not have been applied`, and nobody has the row. A `tsvz stop` request flushes the queued
  batches, with no limit, before it ends the accept loop, so a pop it had queued is answered. From reading; to be
  reproduced first. 0.1.4 drains the same way.
- Why: `finish_connections` gives requests already read `FINISH` (5 s) and then ends every connection still open;
  `pop` and `popitem` delete the row, so their answer is the only copy of it.
- Revisit when: 0.2.1. The plan: reproduce with a stalled write; then wait for a request whose batch is being
  written with a bound of its own, whose reason is written next to it (a stalled disk must not hold a stop forever),
  keeping the five seconds for a client that does not read its answer.
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

### A routed pop whose row cannot be printed, with the handler gone, loses the row
- Kind: deferred
- What the operator sees: a `pop` or `popitem` through a handler whose standard output cannot be written (a closed
  pipe, a full disk) puts the row back with `setdefault`, over the same connection. If the handler has hung up since
  (a stop in its finish window, or it is gone), the put-back fails: the command exits 1 and says `the row taken could
  not be printed nor put back (...): ROW`, and the row is gone from the store, named on stderr only. On the files a
  pop prints before it deletes, so it never loses a row this way. 0.1.4 does the same.
- Why: the handler writes the row's tombstone before it answers, so the command can undo the pop only through the
  handler.
- Revisit when: 0.2.1. The plan: when the put-back fails because the handler is gone, redo the `setdefault` on the
  files (no handler is left to race with).
- Noted: 2026-11-17, in the 0.2.0 re-review; a deferral under the user's standing rule that deferrals are noted, not
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

### A read that falls back during a stop warns that it may or may not have been applied
- Kind: deferred
- What the operator sees: a `read`, `get`, `has`, `len` or `keys` that loses the connection to a stopping handler
  runs on the files and gives the right answer, exit 0, but warns `lost the handler (Connection reset by peer (os
  error 104)); the request may or may not have been applied; using the files directly`, though a read applies
  nothing. Seen in 16 of 200 `get`s racing a stop. 0.1.4 says the same.
- Why: the fallback warns with the whole `HandlerLost` error, whose text is written for a write.
- Revisit when: 0.2.1. The plan: for a read-only operation, warn with the I/O reason alone.
- Noted: 2026-11-17, in the 0.2.0 re-review; a deferral under the user's standing rule that deferrals are noted, not
  asked.

### Two older handler tests fail now and then when the whole test binary runs in parallel
- Kind: deferred
- What the operator sees: nothing; a test run. Running the handler test binary again and again with its tests in
  parallel, `a_client_never_reads_an_answer_meant_for_a_request_it_gave_up_on` failed in 4 of 940 runs, at its first
  assertion with `Ok(Some(["a", "1"]))`: the `get` made while the handler had been sent SIGSTOP was answered. In 120
  runs of a variant, `a_zero_timeout_is_the_shortest_wait_not_none` failed once (its panic text was not kept). Both
  tests are older than 0.2.0.
- Why: not diagnosed. For the first, the stop signal may take hold of the handler's threads after it has answered.
- Revisit when: 0.2.1. The plan: run each in a loop beside the other tests with the panic text kept, and fix the
  test, or the code if it is wrong.
- Noted: 2026-11-17, in the 0.2.0 re-review; a deferral under the user's standing rule that deferrals are noted, not
  asked.
