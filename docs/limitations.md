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

