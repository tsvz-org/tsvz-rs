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

### A lone key is a deletion to tsvz-rs at one column, a row to TSVZ.py, so keys list in another order after a TSVZ.py rewrite
- Kind: deferred
- What the operator sees: where TSVZ.py and 3.39 read a key with no value as a row at one column, tsvz-rs reads it as a
  deletion. TSVZ.py's rewrites (a commit, a scrub, a rewrite, a delete and a rewrite, a delete and its commit) keep a file the
  same for TSVZ.py's readers, but tsvz-rs can then list the keys in another order, with no key or cell lost. Examples, run
  with the tsvz-rs 0.1.3 binary and TSVZ.py 5.10:
  - A commit: `d\tX\tW\na\nd\t\ne\t1\nd\t\n` in a `TSVZed` given header `q`, `del t['a']` and its commit write
    `d\tX\tW\nd\ne\t1\nd\t\n`. TSVZ.py lists d, e; tsvz-rs lists e, d.
  - A scrub: `id\nb\t1\tX\tX\na\na\t2\nd\tX\tX\t2\na\t\n#_defaults_#\tP\t\tQ\n` with no header and defaults
    `['#_defaults_#', '', '']` writes `id\nb\t1\tX\tX\na\nd\tX\tX\t2\na\t\n#_defaults_#\tP\t\tQ\n`. tsvz-rs lists b, a, d
    before it and b, d, a after; TSVZ.py lists b, a, d both times.
- Why: the two read a lone key at one column differently, and TSVZ.py's compaction orders lines for its own reading.
- Revisit when: the next tsvz-rs version. The plan: read a lone key as a row at one column, as TSVZ.py does.
- Noted: 2026-11-09, found in the TSVZ 5.10 re-review (round 3); a deferral under the user's standing rule that deferrals are noted, not asked.
