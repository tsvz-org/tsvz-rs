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
- Revisit when: TSVZ 5.19 resumes, together with TSVZ.py (moved from 0.2.2 and from 0.2.4: TSVZ 5.19 is paused at
  the user's ask, and tsvz-rs alone cannot take the new lock, below). The plan, measured on all three kernels: a
  handler holds a write lock (an open file description lock) on one byte far past the pointer file's content
  (offset 2^62), where no reader reads, and treats EACCES and EAGAIN as busy; readers then read the pointer, and a
  second handler is refused. Both tools must change together: on a local file system `flock` and such a lock do
  not exclude each other, so a handler of one tool that took the new lock would not exclude a handler of the other
  that takes `flock`. TSVZ.py's lookup must also catch an error of the read. The append lock's range is a spec matter (§18.6)
  and goes to the TSVZ session; with a reachable handler the command line reads through its socket and never meets
  it.
- Noted: 2026-11-17, in the 0.2.0 build; measured and planned 2026-11-23, in the 0.2.1 build; moved to 0.2.3 on
  2026-11-25, to 0.2.4 the same day (0.2.3 is the musl build fix alone), and out of 0.2.4 the same day (0.2.4
  takes the deferrals tsvz-rs can settle alone); a deferral under the user's standing rule that deferrals are
  noted, not asked.

### A routed `pop` or `popitem` can lose its row when its answer does not arrive
- Kind: deferred
- What the operator sees: the handler deletes the row, then writes its answer, the row's only copy. Two windows
  are left. (1) The handler's process dies (killed, out of memory) after the deletion is written and before the
  answer is: the row is gone and the command exits 1 (`lost the handler`), naming no row. (2) The answer is
  written and never read (the command is killed in between): the row is gone. When the answer cannot be written
  (the client hung up, gave up waiting, or did not read an answer too large for the socket's buffer before a stop cut it off), the handler puts the row
  back (0.2.2 while it serves, 0.2.4 also while it stops, on the files; 0.2.1 lost it), unless a row was written
  for its key since, which stays (the row taken is then printed on `serve`'s standard error).
- Why: the pop is delete, then answer; a written answer is all the handler can see.
- Revisit when: TSVZ 5.19 resumes, with the TSVZ session: a client acknowledgement for `pop` and `popitem` (§21.7,
  beside §21.9), which both tools' handlers and clients take up together, with the deletion written after the
  acknowledgement (a handler that dies before it then leaves the row; one that dies after it and before the
  deletion leaves a row that was delivered); or accept (1) and (2) as a known limitation (the user's OK).
- Noted: 2026-11-23, in the 0.2.1 final review; narrowed 2026-11-25 in 0.2.2 (the put-back on a failed write) and
  in 0.2.4 (the put-back during a stop, on the files); a deferral under the user's standing rule that deferrals
  are noted, not asked.

### A stop's put-back of a cut-off `pop` is made on the files, beside the engine
- Kind: deferred (0.2.5)
- What the operator sees: nothing different from a put-back made while serving: a `pop` or `popitem` whose answer a
  stop cuts off has its row put back with `setdefault` (which keeps a row written for its key since) before `tsvz
  stop` returns. The stop makes it by a write path of its own, on the files, after the engine is closed
  (`Engine::put_back`: a read of the files and an append, synced, one put-back at a time under the writer thread's
  mutex), where a put-back while serving goes through the engine's queue. Two findings of 0.2.4's rounds were in
  that path (a put-back made before the writer's last batch overwrote an acknowledged write: 13 of 3,000 forced
  stops; two put-backs of one key both appended: 55 of 64), both fixed there; the path stays a part with an
  interleaving of its own.
- Why: `finish` closes the engine before it settles the pops' answers, so a cut-off pop's put-back finds the
  engine closed. Closing it later needs the stop's order changed (its tests, the two forced-stop harnesses and a
  review again), which is a change of its own, not a patch of the round that found the second finding.
- The plan (0.2.5): `finish` closes the engine only after the connection threads have ended, with `begin` refusing
  every request after the first cut (what the early close does today: a spared connection begins nothing new). A
  cut-off pop's put-back then goes through the engine, under its state mutex and in its queue, as it does while
  serving. The files path (about 35 lines), `close`'s wait for a second caller and the hold that makes the
  put-backs one at a time (`Engine::end`) go, and the stop's last write is still the queue's, synced by `close`.
- Revisit when: 0.2.5.
- Noted: 2026-11-25, in 0.2.4's re-read (the step back after the second finding in the part); a deferral under the
  user's standing rule that deferrals are noted, not asked.
