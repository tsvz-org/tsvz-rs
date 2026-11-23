# Lessons

What this project's past changes taught us. Read all of this before changing an API, a format or an
agreed behaviour here. For the full story of a line, find the entry with the same date and name in
`lessons-full.md`.

When one has to change, add the full entry to `lessons-full.md` and one line here, newest last, in the
same commit as the change. Never merge lines to save space; before this file passes 80 KB (what one
Claude Code read returns, measured 2026-11-15), ask the user.

- **2026-11-15 Loose files are read and rewritten by TSVZ.py's rules where the two can agree**: when you read and rewrite files another program owns, read them by its rules wherever you can, and let a rewrite keep lines as they are, changing one only where every reader reads the result alike; decide a write from the same input its rules decide from, since a state they do not have is one your reading can get wrong.
- **2026-11-17 macOS takes flock; the lock-handle table goes**: between processes, take a lock that belongs to the open file (an open file description lock, `flock`), as the program you share the file with does; a lock that any close in the process releases cannot be kept by bookkeeping in a library, since every open of the file in the process would have to go through it.
- **2026-11-17 One member size per batch**: honour a SHOULD at the grain the other implementation does, and the unit your writer already has; a finer grain is state kept for a case that no reader can tell apart.
- **2026-11-17 A stop closes the socket**: when a server stops, end the connections it will not serve the way clients already handle a server that goes away, not with a new answer: a new answer works only on the client paths taught it, and the one left untaught fails.
- **2026-11-17 maint::snapshot is crate-private**: make a function public only when a caller outside needs it; a public helper must answer for inputs its own callers never give it, and that answer is code and a promise to keep.
- **2026-11-23 A loose file is read by TSVZ.py's line rule whole**: when you read files another program owns and its rule is a single function, port that function whole, with the other program as the test oracle, rather than the part a bug touched: the edges between two rules are where the findings keep coming from.
- **2026-11-23 A read begins at the last snapshot part, and a look only chooses where**: when a cheap look guides an expensive read, let the look only choose and have the read confirm from what it decodes anyway, falling back when it does not: the look then needs no rules of its own, and cannot make the result depend on how it looked.
- **2026-11-23 A stopping handler keeps its listener until its queue is written**: a server that stops goes on closing new connections unread until the work they could race with is written, and says it is gone only after; and it never cuts off an answer that is the only copy of what its write removed, since the stop makes that write anyway.
- **2026-11-23 A stop exits by whether the handler is gone**: a command that asks for an outcome waits for that outcome, whoever brought it about, and reports a wait that runs out as not reached, never as success.
- **2026-11-23 A writer checks that what it writes reads back**: when a port replaces a refusal with an encoding, run the reader's own rule over the output for every input the refusal covered (a delimiter that is a letter of the escape token, a key that reads as a comment), since an encoding is a promise that is only as wide as the cases it was tried on, and an exemption ("a tombstone deletes nothing") has to hold in every file shape, one column too.
- **2026-11-23 An optimistic check guards the property, not the stamp**: when a write is made only while the file is as it was read, check what the decision depended on (the same file, no shorter, its last bytes as they were), not a stamp of everything, which refuses every change, including the appends that cannot matter, and so fails under the load it was meant to tolerate.
