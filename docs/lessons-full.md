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
- Why we had to change: the table cost 278 code and 240 test lines (the net the removal took out, with
  `src/` above each file's `mod tests {` counted as code and the rest of `src/` and `tests/` as tests; the
  question had estimated about 350 and 225), a CI step, delayed closes and four fixes, and asked every
  program embedding the library to route its opens through it; it guarded the classic lock on macOS,
  where `flock` does the job, and on Linux before 3.15, where it was left to the known limitation (the
  user's pick 2026-11-17, "Switch, accept it").
- Original design was wrong because: it took the lock kind macOS has in common with Linux's old kernels
  (a classic POSIX lock, owned by the process) and then had the whole process cooperate to keep it, where
  a lock owned by the open file needs no cooperation; TSVZ.py moved to one (`flock`) in 5.3.
- Lesson: between processes, take a lock that belongs to the open file (an open file description lock,
  `flock`), as the program you share the file with does; a lock that any close in the process releases
  cannot be kept by bookkeeping in a library, since every open of the file in the process would have to
  go through it.

## 2026-11-17 One member size per batch
- Changed: a compressed batch is written at one member size, also when it sets `#_member_size_#`: a
  handler's batch at the size in force where it starts, `Writer::append` at the writer's size. A handler
  applies the size a batch sets from its next batch; a `Writer` keeps the size it was built with, and
  `set`, `append` and `delete` on the files write at 1 MiB, as before (TSVZ.py writes a `delete` of a key it
  strips at the size in force: only the members' layout differs). 0.1.x split a batch into runs at each
  such marker, so the records after it took its size at once.
- Why we had to change: the runs cost code in the writer and in the handler's batcher, and a test for each
  split, for a case only a store being set up meets (a client that sets the member size and writes in
  the same batch); §16.7 is a SHOULD, and TSVZ.py writes a batch at one size (the user's pick
  2026-11-17, "Both (Recommended)").
- Original design was wrong because: it honoured a SHOULD at a finer grain than the reference
  implementation, run by run at each marker, where the batch is the unit a writer already has.
- Lesson: honour a SHOULD at the grain the other implementation does, and the unit your writer already
  has; a finer grain is state kept for a case that no reader can tell apart.

## 2026-11-17 A stop closes the socket
- Changed: a handler that stops closes the connections it will not serve (those still waiting to be
  accepted, accepted once it is stopping, or whose request comes after the stop) as TSVZ.py's does,
  instead of answering them "the handler is stopping"; the refusal and its five limits (128 clients, 1 s,
  200 ms, 16 reads, 64 KiB) go. A routed read that meets a stop now runs on the files and succeeds
  (before, it exited 1); a write exits 1 saying it may or may not have been applied. A request the
  handler had received before the stop is served and answered, for up to 5 s (on Linux this includes one
  still unread on the connection, which macOS drops). The command line holds SIGPIPE off around its
  writes to a handler's socket, since std passes `MSG_NOSIGNAL` to a Unix socket's writes only on Linux
  and only from Rust 1.90 (a write to a closed socket ends a Mac's process with Rust 1.100 too); the
  crate builds from 1.85.
- Why we had to change: the command line redoes a read on the files when the connection is lost during the
  request (and runs every command there when none can be made), but not when the handler answers that it
  is stopping, so the refusal made routed reads fail during a stop; the refusal cost five limits and the code
  and tests that held them (the user's pick 2026-11-17, "Close the socket (Recommended)").
- Original design was wrong because: it gave a case clients already handle (a handler that goes away)
  an answer of its own, which the read's fallback never learnt; and an answer has to be written before
  the request is read, which needed the limits.
- Lesson: when a server stops, end the connections it will not serve the way clients already handle a
  server that goes away, not with a new answer: a new answer works only on the client paths taught it,
  and the one left untaught fails.

## 2026-11-17 maint::snapshot is crate-private
- Changed: `tsvz::maint::snapshot` is no longer public, and its branch for loose files goes with the
  doc promise about it (0.2.0, with the other breaking changes of the library).
- Why we had to change: nothing outside the crate called it, and inside it is called only on strict
  stores, so its loose-file branch was code and a documented promise for a case no caller reached
  (chief's decision of 2026-11-15 kept it public in 0.1.4, "revisited at 0.2").
- Original design was wrong because: it made a helper of `scrub` public with the rest of the module,
  so it had to answer for every store a caller might pass, not only the ones `scrub` passes.
- Lesson: make a function public only when a caller outside needs it; a public helper must answer for
  inputs its own callers never give it, and that answer is code and a promise to keep.

## 2026-11-23 A loose file is read by TSVZ.py's line rule whole
- Changed: tsvz-rs reads a loose file as TSVZ.py's `_processLine` does: keys and cells without the
  whitespace at their end and decoded (`<sep>`, `<LF>`), `#_defaults_#` lines that fill the empty
  cells after them, a key of only whitespace skipped, a row wider than the column count cut of its
  empty end cells, the unterminated last line read; a requested key is taken without its end
  whitespace, a missing key has no row, records print cells encoded as TSVZ.py prints them, and
  writes encode cells so (a delimiter or newline is no longer refused). A rewrite plans from the same
  parse, so its guard for lines "the two may read differently" went.
- Why we had to change: 0.1.4 took TSVZ.py's column count and deletions but kept its own cell and key
  rule, and the reviews of 0.1.4 and 0.2.0 found difference after difference between the two: six
  ways a read differed, three edges where tsvz-rs named a key otherwise in its read, its rewrite and
  its follow state, a delete beside a clear, and every rewrite of a file with a trailing space, a `<`
  or a CR left uncompacted by a cautious guard.
- Original design was wrong because: it ported the part of the other program's rule each finding
  touched, so two half-rules met at every edge, and each edge needed state of its own (an odd-line
  kind, a stand-in key spelling, a blank-key special case in delete) that the next review found wrong
  in another case.
- Lesson: when you read files another program owns and its rule is a single function, port that
  function whole, with the other program as the test oracle, rather than the part a bug touched: the
  edges between two rules are where the findings keep coming from.

## 2026-11-23 A read begins at the last snapshot part, and a look only chooses where
- Changed: a read of a multi-part store begins at its last snapshot part (§19.10.3); `verify` and a
  rewrite's read replay every part. The part to begin at is chosen by a first look at each part's
  first lines, newest first, and confirmed by the read of that part, from the lines it decodes.
- Why we had to change: reading every part cost about 11 times as much on 20 parts of history, and
  0.1.4's attempt, a look inside the read, decoded each part's start twice and left bzip2 out, so the
  answer depended on the codec.
- Original design was wrong because: the look decided where the read began, and the read trusted it;
  making that look exact meant a second decoder held to the read's rules.
- Lesson: when a cheap look guides an expensive read, let the look only choose and have the read
  confirm from what it decodes anyway, falling back when it does not: the look then needs no rules of
  its own, and cannot make the result depend on how it looked.

## 2026-11-23 A stopping handler keeps its listener until its queue is written
- Changed: a handler that stops goes on accepting connections, and closes each unread, until every
  queued write is written; it then removes its pointer file, and only then closes its listener. A
  `pop` or `popitem` still in flight when the stop's 5 s end is no longer cut off: the stop writes
  its tombstone, as before, and its answer is given 5 s more to be written; the other connections
  are cut off then, and a connection cut off begins no other request. Before, the listener closed
  first and every connection was cut off at 5 s.
- Why we had to change: a command that found the listener closed ran on the files, where a `pop` read
  a row as live that a `pop` in the handler had taken and whose tombstone was still queued, so two
  clients got one row; and a pop cut off at 5 s lost its row, whose tombstone the stop went on to
  write (deferrals of the 0.2.0 final review, built in 0.2.1).
- Original design was wrong because: it closed the handler's front door before the work it still
  held was written, so the clients' fallback ran against files that did not yet show that work; and
  it cut connections off by time alone, though the stop makes their requests' writes anyway, so for
  a pop the cut lost the only copy of the row.
- Lesson: a server that stops keeps telling new clients it is gone (closing them unread) until the
  work they could race with is written, and announces it is gone only then; it never cuts off an
  answer that is the only copy of what its own write removed, since the stop makes that write anyway.

## 2026-11-23 A stop exits by whether the handler is gone
- Changed: `tsvz stop` (and `handler::server::stop`) exits 0 once the handler's pointer file no longer
  names it, also when another stop, a signal or the idle timeout stopped it first: a connection the
  handler closes unread means it is stopping, and the stop waits for it as the first stop does. It
  exits 1 when the handler is still there 30 s after the request, the wait for its answer included.
  Before, a stop that lost the race exited 1 with "does not answer" or "went away", and every stop
  waited at most 10 s for the handler to go and then exited 0 whether it had or not; the wait for the
  answer had no limit.
- Why we had to change: two stops that overlap both mean "stop it", and the handler stops; one of them
  failed (48 of 150 overlapping pairs on 0.2.0, under 24 clients). And a stop that exits 0 is read as
  "it is gone", which the 10 s wait did not check (0.2.0 re-review deferrals, built in 0.2.1).
- Original design was wrong because: it reported which request had reached the handler, not the
  outcome the operator asked for, and it ended its wait with success when its time ran out (TSVZ.py's
  `_cliStop` does the same).
- Lesson: a command that asks for an outcome waits for that outcome, whoever brought it about, and
  reports a wait that runs out as not reached, never as success.

## 2026-11-23 A writer checks that what it writes reads back
- Changed: a loose `set` (and every loose write through `Writer`) refuses a cell whose encoding still holds the
  delimiter (`-d e` and `set k hello`: `<sep>` has an `e`, so the line read back as `h<s` and `p>llo`), and a key that
  could never be read back: one that begins with `#` (a comment), begins with a NUL (a reader drops it) or is only
  whitespace (no key). A key alone is refused too, since in a file of one column (or with no keyed line yet) it is a
  row; `delete` drops the names no row can have, so a delete of one writes nothing and is no failure. Before, 0.2.1's
  build wrote all of them with exit 0.
- Why we had to change: 0.2.0 refused any delimiter in a cell, which made a cell holding a delimiter unwritable;
  0.2.1 encoded it as TSVZ.py does (`<sep>`) and so dropped the refusal, but the encoding itself holds the letters
  `s e p L F < > /`, which a delimiter can be, and the keys a reader skips were written as before (0.2.0 wrote
  them too; the whitespace key was a 0.2.1 regression, since 0.2.0 read such a key). Found by the second opinions of the
  0.2.1 final review (R07a-1, R08-1 to R08-3), built in the fix round.
- Original design was wrong because: the encoder was tested on the cells it was meant to carry, not on the
  inputs the refusal it replaced had covered, and nothing asked the reader's own rule whether the line it was about to
  write gave the key and cells back. The first fix round then exempted a key alone ("a tombstone deletes
  nothing"), true of the file it had in mind (more than one column) and false of a file of one column, which the
  re-review found; the exemption was a second encoding of the same promise, tried on one file shape.
- Lesson: when a port replaces a refusal with an encoding, run the reader's own rule over the output for every input the
  refusal covered; an encoding is a promise only as wide as the cases it was tried on.

## 2026-11-23 One rule names a loose key: the reader's
- Changed: every key a loose command names (`get`, `has`, `pop`, `setdefault`, `delete`, bulk input, a keyed
  read) is named by `loose::name`: the cell the reader reads from the key as it is written (encoded, trimmed of the
  whitespace at its end, decoded). `$'a\n'` names the line `a<LF>`, a blank at a key's end still goes, and a key no
  row can have (`#x`, `</sep/>`, `-d e` and `hello`) names none. A loose write reads its own line back with
  `loose::parse` and refuses it when the cells are not the record less that trim; that replaced three hand-written
  lists (`unwritable`, `delimiter_in_encoding`, `unreadable_key`). Bulk input of a loose format is read with
  `loose::parse`, setdefault's marker check names its key first, and the header drops the NULs a reader drops.
- Why we had to change: 0.2.1's fix rounds named a key by trimming its decoded form, where the reader trims the
  encoded cell before decoding. On a file holding rows `a` and `a<LF>`, `delete $'a\n'` deleted `a`, and `get`,
  `has` and `pop` acted on `a` (0.2.0 refused or exited 3); `popitem` took the same row forever. The step back of
  the second fix round's re-review found every finding of all rounds (R07a-1, R16-1, R08-1 to 3, A, B, R07a-3,
  R10-3, F4, F5) in a copy of the reader's line rule outside the reader.
- Original design was wrong because: each command that named a key wrote its own short form of the reader's rule
  (a trim, a list of refusals, a comment test), each right on the cases it was tried on, and each drifted from the
  reader on the ones it was not: a line break is whitespace once decoded but not as written.
- Lesson: when several places must agree with a reader about what a name or a line means, give them the reader's
  own function (encode, read back, compare) instead of a description of it; a copy of a rule is a second rule.

## 2026-11-23 An optimistic check guards the property, not the stamp
- Changed: a loose `delete` at more than one column appends its key alone to the file it read, or to that file with
  more appended since by another writer: the writer checks the file's inode, that it is no shorter, and its last
  4 KiB, instead of the part's stamp (inode, size, mtime). Before, any change refused the delete (three tries, then
  `delete skipped`), and a `pop` had already printed its row.
- Why we had to change: the stamp was added so that a `clear` between the read and the append cannot turn the key
  alone into a one-column row; it also refused every other writer's append, which changes size and mtime, though the
  decision (the file's column count, set by its first keyed line) is the same after an append. 0.2.0 appended
  whatever happened; the check made a busy file harder to delete from than that.
- Original design was wrong because: the check was written for the case that found the bug (a clear) as an equality on
  everything the file has, not on the property the decision rested on.
- Lesson: when a write is made only while the file is as it was read, check what the decision depended on (here: the same
  file, no shorter, its end as it was), not a stamp of everything; a stamp refuses every change, the harmless ones
  too, and so fails under the load it was meant to tolerate.

## 2026-11-25 A handler is gone when no process holds its pointer file
- Changed: `tsvz stop`, and a routed `pop` that puts its row back on the files, take a handler for gone once its
  pointer file no longer names it by pid and address, or once no process holds the file's `flock` (the system lets
  go of it when the handler's process ends); a stop finds no handler to stop in a pointer file no process holds. A
  stop whose connection is closed unread asks once more, 100 ms later, before it waits for the handler to go.
- Why we had to change: 0.2.1 told a gone handler by the pointer file's name and pid alone. A handler killed after
  it answered a stop, or a pop, left its file, which still named it: the stop waited 30 s and failed (0.2.0 exited 0
  after 10 s), and the pop's command waited 30 s and printed its row on standard error instead of putting it back;
  another handler with the same pid (reused) was taken for the one asked; and a handler that closed one connection
  for want of a descriptor or a thread was taken for one stopping (0.2.1's final review).
- Original design was wrong because: it judged a process by a record the process wrote, which outlives a process
  that is killed, and whose pid the system gives to the next one; the handler already held a lock that the system
  releases at exactly the moment that matters.
- Lesson: to tell whether the process behind a file is still there, ask the kernel lock it holds on that file,
  released when the process ends, not the name, pid or address it wrote; a record outlives the writer that is
  killed before it removes it.

## 2026-11-25 A stop reports a lost write it had acknowledged
- Changed: the handler keeps the first failure that lost a write it answered before it made it (a failed batch of
  such writes, a failed sync of them); every later `stop` it answers gets 1 with it (`tsvz stop` waits for the handler
  to go, then exits 1; a stop that finds it already stopping is closed unread, and exits by whether it goes), and
  `serve` ends with status 1, also for a write lost while it stops. 0.2.1's stop flushed the queue and answered `#0`
  if that flush worked, and `serve` exited 0.
- Why we had to change: a stop that answers `#0` says every acknowledged write is durable (§21.9.3); a write lost in
  a batch that failed earlier is not, and a later batch that works does not bring it back (0.2.1's final review).
- Original design was wrong because: the stop judged by its own last step, the flush, and the failure state it read
  (`failing`) was cleared by the next batch that worked.
- Lesson: a command whose success promises that earlier work is safe keeps the first failure that broke the promise
  until it is asked, as state of its own; a later success clears an error banner, not a loss.

## 2026-11-25 An append to a compressed loose file begins with a newline
- Changed: each member tsvz-rs appends to a compressed loose file that holds something begins with `\n`, an empty
  line both tools read as nothing, so a last line without a newline is ended instead of joined (TSVZ.py's append joins
  it). A plain loose file is ended as before, by looking at its last byte.
- Why we had to change: `b<TAB>1` without a final newline, then `delete b`, read `b<TAB>1b`: the record was lost and
  the row changed (0.2.1's loose differential test).
- Original design was wrong because: it took the end of what it appended to for a line end, which it can check in a
  plain file and cannot in a compressed one without decoding the whole file.
- Lesson: when an append cannot see the end of what it appends to, begin it with a separator the reader ignores
  rather than trust that end.

## 2026-11-25 setdefault prints the row a reader reads back
- Changed: `setdefault` of a missing key in a loose file prints the row `get` reads after the append, and exits 3 with
  no row where the line it wrote is a deletion, as TSVZ.py's does; 0.2.1 printed the cells given, padded with blanks.
- Why we had to change: a value's trailing blank, the cells a `#_defaults_#` line fills, and a value list of blanks
  (a deletion at more than one column) printed a row no reader reads (0.2.1's short round).
- Original design was wrong because: the print was computed by a second copy of the reading (the strict store's
  padding), which the loose reader's rule had outgrown.
- Lesson: to show what a write made, read it back through the reader every other command uses; a computed copy is a
  second rule.

## 2026-11-25 A loose scrub keeps what TSVZ.py's strict reader reads
- Changed: a loose `scrub` keeps the last line of each key that TSVZ.py's strict reader takes (it skips a line
  that holds a value but is narrower than the column count), and moves a key's last line into its first line's
  place only where that reader takes the last line, or none of the key's lines, where no `#_defaults_#` line holds
  a value (that case stays in docs/limitations.md: such a line makes the reader take a narrower line, which the
  plan does not follow; the order in which that reader meets the keys can still change, also there).
- Why we had to change: `#h`, `x 0 0`, `K 1 2`, `K 9` read `K` as `[K, 1, 2]` to that reader and had no `K` after a
  scrub; on 3,000 random files 0.2.1 changed that reader's reading of 78. TSVZ.py's scrub promises that every
  reader given the scrub's header, defaults and column count reads the file as before.
- Original design was wrong because: the scrub's plan named the readers it kept (the command line's, and one given
  line 1 for its header) and left out a reader of the same files that skips lines.
- Lesson: list every reader that reads the files you rewrite, strict and lenient, and test the rewrite against each
  on random files; a reader left off the list is the one whose reading changes.

## 2026-11-25 A handler gives back a popped row whose answer it cannot write
- Changed: when the handler cannot write the answer of a `pop` or `popitem` (the client hung up or gave up), it puts
  the row back with `setdefault`, which keeps a row written since (and prints the row taken then, as an error that
  `-q` shows); one that is stopping puts nothing back and prints the row. 0.2.1 lost the row.
- Why we had to change: the handler deletes the row before it answers, so the answer is the row's only copy (0.2.1's
  final review).
- Original design was wrong because: it treated a failed answer as the client's problem, though the write it answered
  for had already removed the data the answer carried.
- Lesson: when an answer is the only copy of what its request removed, undo the removal when the answer cannot be
  delivered; only an acknowledgement closes the window after delivery.

## 2026-11-25 A loose clear writes the empty line 1 TSVZ.py's does
- Changed: `clear` of a loose file writes an empty line 1 where line 1 was no comment (a row, a defaults line, or
  nothing), as TSVZ.py's `clear` does. A comment line 1 stays (TSVZ.py's `clear` replaces it too; both tools read the
  two files alike). 0.2.1 kept the `#_defaults_#` lines from line 1 on, so a defaults line, or nothing, was line 1.
- Why we had to change: a TSVZ.py program that reads a loose file with a header takes line 1 for it: after a 0.2.1
  `clear` that was a defaults line, or the first row written later (0.2.1's final review).
- Original design was wrong because: the clear was planned from what the command lines read, which agree, and the
  bytes of line 1 were left to follow from that; to a program that reads by position, line 1 is part of what the
  file says.
- Lesson: when another program reads the files you write by position (line 1 as a header), write that position as
  it does, even where every reader you test reads the result alike.

## 2026-11-25 A request's own `#_write_ack_#` is durable itself
- Changed: a handler answers a request that sets `#_write_ack_#` (either value, `set -` with such a line among its
  records) once its own writes are synced. 0.2.1 decided from the markers in force when the request arrived, so a
  `disk` set by the request, followed by `k<TAB>v`, was answered at once and `k` not synced, though the files said
  `disk` once it was written.
- Why we had to change: a client that asked for `disk` was told its write was done, and could lose it (0.2.1's final
  review).
- Original design was wrong because: durability was a fact about the state before the request, and a request can
  change that state before its own writes are made.
- Lesson: when a request can change the rule that applies to the writes around it, take the stricter reading where
  the spec is silent (§21.9.2 is silent; TSVZ.py decides from the markers it last read, and tsvz-rs departs from it
  here): a sync costs time, a write lost after its answer costs the promise.

## 2026-11-25 A request the handler cannot parse is warned of failing writes too
- Changed: while a handler's writes are failing, an answer with status 2 (a request it cannot parse: an unknown option
  or operation, the wrong arguments, `setdefault` of a marker key) carries the `#!` warning that writes acknowledged
  before they were written may be lost, as every other answer but `stop`'s does, and as TSVZ.py's do. 0.2.3 gave
  those none.
- Why we had to change: a client that sends a request the handler refuses is told nothing of the writes it lost,
  where TSVZ.py's handler tells it (0.2.2's last re-read).
- Original design was wrong because: the warning was added where a parsed request is answered (`Engine::handle`),
  and a request that fails to parse is answered before that, in the connection's loop.
- Lesson: a word that every answer must carry is added where every answer is made, not where most of them are: an
  answer made on a path of its own (a refusal before the work) is the one that goes without it.

## 2026-11-25 A stopping handler puts a popped row back on the files
- Changed: when a pop's answer cannot be written once a stop has closed the engine (a client that does not read it
  is cut off after the stop's second window; one that hung up), the handler puts the row back with `setdefault` on
  the files, as any other writer, synced, and `serve` ends after it. 0.2.2 and 0.2.3 printed the row on `serve`'s
  standard error and left it deleted.
- Why we had to change: the answer the stop cut off was the row's only copy (0.2.2's final review, the third window
  of the pop's lost row).
- Original design was wrong because: the put-back went through the engine alone, and the stop closes the engine
  before it waits for the pops' answers, so the put-backs the stop itself causes had no writer left.
- Lesson: when a shutdown can cause the failure that a repair handles, leave the repair a way to write after the
  shutdown's own writer has closed (as any other writer of the files would), and end the shutdown after it.

## 2026-11-25 A serve that takes over from a killed handler removes the socket it left
- Changed: a `serve` that takes over a pointer file no process holds (its handler was killed) removes the socket the
  file names, and then the directory made for it if empty, when the socket is this user's, on this host, named `s`
  in a `tsvz-` directory, and nobody listens on it (a connection to it is refused). 0.2.3 left both for good, under
  `$XDG_RUNTIME_DIR` or the temporary directory.
- Why we had to change: each handler that was killed left a directory behind (0.2.2's build).
- Original design was wrong because: the clean-up was in the handler's own stop, and a killed process runs none; the
  one that learns the handler is gone is the next one to take over its pointer file.
- Lesson: leave the clean-up of what a killed process made to whoever next finds it dead by the kernel's word (its
  lock let go of), and remove only what that process's own layout makes and the system shows unused (a refused
  connection), since the record that names it is a file others can write.

## 2026-11-25 A loose file tsvz-rs creates begins with the empty line 1 TSVZ.py writes
- Changed: `set`, `append`, `setdefault` and `delete` on a missing loose file create it with an empty line 1 (a
  newline; compressed, inside the first member), as TSVZ.py creates one; 0.2.3 wrote the first row as line 1, and
  `delete` left the file empty. A file that exists, empty, gets none, as in TSVZ.py.
- Why we had to change: a TSVZ.py program that reads the file with a header took its first row for the header
  (0.2.2's final review, F6).
- Original design was wrong because: 0.2.2 wrote TSVZ.py's line 1 where a `clear` makes it, and left the other place
  that makes line 1, the writer's first append to a file it creates, as it was.
- Lesson: when you copy how another program writes a position (line 1), copy it at every path that makes that
  position: a rewrite and the first write to a new file both do.
