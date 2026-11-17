# Lessons: full entries

Why this project's APIs, formats and agreed behaviours had to change. `lessons.md` has one line per
entry: read that first, and look entries up here by date and name rather than reading this whole. New
entries go last, in the same commit as the change:

```markdown
## YYYY-MM-DD <short name>
- Changed: ...
- Why we had to change: ...
- Original design was wrong because: ...
- Lesson: ...
```

## 2026-11-15 Loose files are read and rewritten by TSVZ.py's rules where the two can agree
- Changed: tsvz-rs takes a loose file's column count from its first keyed line, and reads a key
  alone, or with only blank cells, as a deletion but a row in a one-column file, as TSVZ.py does.
  Its `scrub` of a loose file keeps each line it keeps as the file has it (`#_defaults_#` lines
  included) instead of writing its rows anew; `clear` keeps the `#_defaults_#` lines in force
  (none in a file of one column); a delete decides from the file's column count alone, as TSVZ.py
  does, and in a one-column file rewrites the file without the key's lines. Line 1's byte-order
  mark is part of its key.
- Why we had to change: tsvz-rs's `scrub` and `clear` of a TSVZ.py-managed `.tsv` dropped its
  `#_defaults_#` lines and a one-column file's keys, and tsvz-rs listed keys TSVZed had deleted
  (TSVZ.py writes a deletion as a bare key with its `delete`, and as `key<TAB>` with `set key ''`).
  Fixing both was asked for in 0.1.4 (2026-11-14).
- Original design was wrong because: it read "their data semantics are implementation-defined"
  (§20.2.4) as room for a simpler rule of its own, on files that another program writes and reads
  by its rules. Each tool's rewrite then changed what the other read: tsvz-rs's scrub lost TSVZ.py's
  defaults and one-column keys, and TSVZ.py's scrub reordered or revived keys for tsvz-rs. The first
  fix then added a rule TSVZ.py does not have, to write a delete only for a key that is live: it read
  the whole file, and misjudged every key the two readers read differently (a trailing space, a
  byte-order mark, a reset of `#_defaults_#`).
- Lesson: when you read and rewrite files another program owns, read them by its rules wherever
  you can, and let a rewrite keep lines as they are, changing one only where every reader reads the
  result alike; decide a write from the same input its rules decide from, since a state they do not
  have (whether a key is live) is one your reading can get wrong; test that with the other program
  as the oracle.

## 2026-11-17 macOS takes flock; the lock-handle table goes
- Changed: on macOS the write lock is `flock`, as TSVZ.py 5.3 and later take it (it was a classic POSIX
  lock); `tsvz::lock::Handle`, the process-wide table that kept every descriptor of a locked file open
  until its lock was released, goes, and with it the README's advice to open a store's files through it
  (0.2.0: a breaking change of the library). Where neither an open file description lock nor `flock`
  exists, the classic fallback can be lost by a multi-threaded process, a known limitation the user
  accepted.
- Why we had to change: the table cost about 350 code and 225 test lines, a CI step, delayed closes and
  four fixes, and asked every program embedding the library to route its opens through it; it guarded
  only macOS, where `flock` does the job (the user's pick 2026-11-17, "Switch, accept it").
- Original design was wrong because: it took the lock kind macOS has in common with Linux's old kernels
  (a classic POSIX lock, owned by the process) and then had the whole process cooperate to keep it, where
  a lock owned by the open file needs no cooperation; TSVZ.py moved to one (`flock`) in 5.3.
- Lesson: between processes, take a lock that belongs to the open file (an open file description lock,
  `flock`), as the program you share the file with does; a lock that any close in the process releases
  cannot be kept by bookkeeping in a library, since every open of the file in the process would have to
  go through it.

## 2026-11-17 One member size per batch
- Changed: a compressed batch is written at one member size, also when it sets `#_member_size_#`: a
  handler's batch at the size in force where it starts, `Writer::append` at the writer's size; the size
  the batch sets applies from the next one. 0.1.x split a batch into runs at each such marker, so the
  records after it took its size at once.
- Why we had to change: the runs cost about 45 code and 110 test lines, in the writer and the handler's
  batcher, for a case only a store being set up meets (a client that sets the member size and writes in
  the same batch); §16.7 is a SHOULD, and TSVZ.py writes a batch at one size (the user's pick
  2026-11-17, "Both (Recommended)").
- Original design was wrong because: it honoured a SHOULD at a finer grain than the reference
  implementation, record by record, where the batch is the unit a writer already has.
- Lesson: honour a SHOULD at the grain the other implementation does, and the unit your writer already
  has; a finer grain is state kept for a case that no reader can tell apart.

## 2026-11-17 A stop closes the socket
- Changed: a handler that stops closes the connections it will not serve (those still queued, accepted
  once it is stopping, or whose request comes after the stop) as TSVZ.py's does, instead of answering
  them "the handler is stopping"; the refusal and its five limits (128 clients, 1 s, 200 ms, 16 reads,
  64 KiB) go. A routed read that meets a stop now runs on the files and succeeds (it exited 1); a write
  exits 1 saying it may or may not have been applied, and nothing of it is made, as before. On macOS a
  client's socket no longer raises SIGPIPE when a handler closes it unread.
- Why we had to change: the command line falls back to the files only when it loses the connection, so
  the refusal made routed reads fail during a stop; the refusal cost about 50 code and 80 test lines and
  five limits (the user's pick 2026-11-17, "Close the socket (Recommended)").
- Original design was wrong because: it gave a case clients already handle (a handler that goes away)
  an answer of its own, which the read's fallback never learnt; and an answer has to be written before
  the request is read, which needed the limits.
- Lesson: when a server stops, end the connections it will not serve the way clients already handle a
  server that goes away, not with a new answer: a new answer works only on the client paths taught it,
  and the one left untaught fails.
