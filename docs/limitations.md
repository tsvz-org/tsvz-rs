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
  before it appends. tsvz-rs reads it as an uncommitted tail, as §4.3 says of a strict store: `read`
  skips it with a warning, and `set` or `delete` cut it first ("cut N uncommitted bytes"), so that
  row is lost. A loose `scrub` keeps it as it is; a loose `clear`, and a delete that rewrites a
  one-column file, keep it too, so TSVZ.py still reads a key tsvz-rs no longer lists.
- Why: tsvz-rs applies the strict-store tail rule to loose files; changing it touches how a store
  follows appends (`Store::refresh` holds the tail until a newline completes it). Found in 0.1.4's
  differential runs against TSVZ.py 5.18: every remaining disagreement was such a file.
- Revisit when: the next tsvz-rs version. The plan: read a loose file's tail as a line, and write a
  newline before an append instead of cutting it, as TSVZ.py does.
- Noted: 2026-11-15, in the 0.1.4 build; a deferral under the user's standing rule that deferrals
  are noted, not asked.
