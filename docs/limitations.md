# Limitations

Approved known limitations and deferrals. Reviewers: read this first, and don't raise these again.

### Rust readers take no lock, so they can see a TSVZ 5.3 rewrite part way
- Kind: deferred
- What the operator sees: TSVZ.py 5.3 readers of a `.tsv`-family file take a shared lock while they read (byte 0 of
  an open file description lock on Linux, `flock` `LOCK_SH` on macOS), so a rewrite under the file lock is waited
  out. tsvz-rs readers (`Handle::open_read` callers) take none: like TSVZ.py 5.2, they can read another process's
  in-place rewrite part way, and a delete made in that moment can be lost. Compatibility needs nothing on Linux:
  tsvz-rs's whole-file write lock conflicts with every 5.3 lock (its appends now also wait for Python readers,
  since byte 0 is inside its whole-file lock: slower, not wrong). On macOS it holds only if `flock` and POSIX locks
  exclude each other there (TSVZ's macOS CI job checks this for TSVZ.py's `lockf`).
- Why: tsvz-rs follows TSVZ.py; the change is a lock-layer change of its own:
  - `Handle::open_read` callers take READ: byte 0 shared on Linux. On macOS that works only after a move to
    `flock`, because a shared POSIX lock would replace the process's own write lock.
  - `maint.rs` rewrites take byte 1, then the whole file; appends move to byte 1, so they do not wait for Python
    readers.
  - Optional: `flock` on macOS would let the `Handle` table and its deferred closes go.
  - Tests: extend `tests/interop.rs` with "a Python READ waits for a Rust rewrite" and "a Rust reader waits for a
    Python rewrite".
- Revisit when: someone shares a file between tsvz-rs and another writer that rewrites it, or TSVZ's macOS gate
  shows `flock` and POSIX locks do not exclude each other on macOS (then tsvz-rs must move to `flock` there with
  TSVZ.py).
- Approved: 2026-11-04, the user chose "Deferred in tsvz-rs (Recommended)".
