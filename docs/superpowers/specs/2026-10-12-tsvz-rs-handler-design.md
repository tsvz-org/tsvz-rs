# tsvz-rs: the write handler

## Intent

Give tsvz-rs the write handler of §21, so that many producers and command-line
invocations share one loaded store and one appender. tsvz-rs then serves stores
itself (`tsvz serve`, `tsvz stop`), and routes ordinary commands through a running
handler (§20.8), whichever of tsvz-rs or TSVZ.py runs it. Rust programs reach a
handler through a small public client.

This is sub-project 2 of 3. It builds on the core crate and CLI (sub-project 1,
`2026-10-12-tsvz-rs-core-design.md`, merged into `main`). The multi-part scrub
(§19.2) stays sub-project 3. The first public release, 0.1.0, waits for this
sub-project and ships both.

In this document `§N` refers to tsvz-spec-v1 (`../TSVZ/tsvz-spec-v1.md`);
"section N" refers to this design's own sections, and "core section N" to the
core design's. "TSVZ.py" is the Python reference implementation in
`../TSVZ/TSVZ.py` (4.6).

### Decisions taken during design

| Question | Decision |
|---|---|
| Rust client | A small public `handler::Client` with typed calls over one connection; no automatic fallback to the files, no reconnect |
| Loose files | Strict stores only: `serve` refuses a loose file, and the CLI never forwards one |
| Release | No separate 0.1.0: the first public release (0.1.0) includes the handler |
| Concurrency | std threads: one per connection, one engine behind a mutex, one writer thread that batches appends; no new dependencies |
| State | Follows the files: writes are appended and come back through `Store::refresh`, so the state is what a reader of the files returns (§21.8.5) |
| Interoperability | Both ways with TSVZ.py: its CLI and `TSVZClient` work against a tsvz-rs handler, and the tsvz-rs CLI against a TSVZ.py handler |
| Where the spec is silent | Behave as TSVZ.py does, and record the choice in `docs/silent-choices.md` |
| Feature claimed | `handler` (conformance class and feature) |
| §20.2.7 (TSVZ 4.7, after this design) | Naming a key: in the shared operations, used by the CLI and the handler (section 2.3) |

### Out of scope

- Loose files (`.tsv`, `.csv`, `.nsv`, `.psv`) through a handler.
- Access from other hosts (§21.3 note); daemonizing (run `serve` under systemd,
  nohup or a shell `&`); Windows.
- Scrubbing a multi-part store: refused through a handler, as on the files
  (sub-project 3).
- TSVZClient's automatic fallback to an in-process engine and its reconnect.

## 1. Layout

```
src/handler/mod.rs       the module: re-exports Client, Response; serve() and stop() for the CLI
src/handler/protocol.rs  request and response lines (section 3)
src/handler/engine.rs    one store's live state and its writer thread (section 4)
src/handler/pointer.rs   the pointer file: take-over, contents, trust checks (section 5)
src/handler/server.rs    socket, connections, stopping (sections 5, 6)
src/handler/client.rs    the public Client (section 8)
src/ops.rs               operations shared by the CLI and the engine (section 2.2)
src/cli/route.rs         forwarding a command line to a handler (section 7.2)
tests/handler.rs         a real `tsvz serve` driven over its socket (section 10)
fuzz/fuzz_targets/request.rs  the request parser (section 10)
```

The handler module is Unix-only (`#[cfg(unix)]`), part of every build (the crate
supports Linux and macOS only), and adds no dependency: `std::os::unix::net` for
sockets and `libc` for `flock`, peer credentials, `gethostname` and signals.
`ops` and the handler internals are `#[doc(hidden)]` like the other internals;
`handler::Client`, `handler::Response` and the new `Error` variants are public.

## 2. Changes to the core

### 2.1 Unreadable parts

§21.8.5 requires a handler whose store has a part it cannot read to answer
`read`, `get`, `has`, `len`, `keys` and `verify` with status 1 and still return
what it could read. Today `Store::open` fails with an `Error` instead (core
section 3.8).

- `Options` gains `skip_unreadable: bool` (default `false`). When it is set, a
  part that cannot be opened or read, or whose codec this build lacks (§16.5),
  is skipped: a `Warning` names it and the reason, the replay continues with
  the next part from the marker state it had reached, and the store records the
  part.
- `Store::unreadable() -> &[PathBuf]` lists the skipped parts, and
  `Store::incomplete() -> bool` is true when a part was skipped or the read
  stopped (§4.10).
- `refresh` treats a skipped last part like a compressed one: any change to it
  means a full reload. Unchanged stamps mean nothing to do.
- A missing store is still `Error::Missing`.
- Library callers keep today's behaviour unless they opt in. The CLI and the
  engine opt in, so the CLI on the files now prints what it read and exits 1, as
  TSVZ.py does and as it already does for a stopped read. Without this,
  forwarded output and output on the files would differ (§20.8.1).
- `maint::clear` and `maint::scrub` always read with skipping off: an unreadable
  part makes them fail with nothing written, as today. `pop`, `popitem` and
  `setdefault` treat an incomplete read like a stopped one: exit 1, nothing
  written (§4.10).

### 2.2 Shared operations (`ops`)

Each operation's result is computed once, by a function both the CLI on the
files and the engine call, so that what a handler answers is what the files give
(§20.8.1). The CLI's `read.rs` and `write.rs` become thin callers.

- `ops::check_args(op, args) -> Result<(), String>`: the arity, `popitem
  first|last` and empty-KEY rules now in `cli::args::parse`. The CLI parser and
  the protocol parser both call it, so a command is a usage error in both places
  or in neither.
- Reading operations take a `&Store` and an output sink (`row(key, cells)`,
  `line(fields)`, `text(s)`) and return the exit status: `read`, `get`, `has`,
  `len`, `keys`, `verify`, `parts`. The CLI's sink is its `Emitter`/`Fields`
  (records or table); the engine's sink builds protocol output lines.
- Writing operations compute and do not write: `pop` and `popitem` return the
  row and the tombstone records; `setdefault` returns the row a reader then
  returns and the record to append, or the live row. The caller appends: the CLI
  through `Writer` after printing (core behaviour), the engine through its
  writer thread before answering.
- `ops::parse_records(bytes, delimiter, strict, keys_only, warn) -> Vec<Record>`:
  the CLI's stdin parser, taking a buffer and a delimiter. The CLI calls it with
  the store's variant; the engine with TAB and §13 decoding (§21.4.2).
- `ops::protocol_record(&Record) -> String`: a record as a protocol line (a
  marker key as written, a data key with `<#>` for a leading `#`, values §13
  with TAB), for the CLI's bulk routing and the client.

### 2.3 Naming a key (§20.2.7, added after this design)

TSVZ 4.7 added §20.2.7 while this design was being planned. While stripping is in
force at the end of a strict store, a KEY whose encoded form (§13) ends in a space
or tab names the live key equal to it if there is one, otherwise the key a reader
resolves it to; any other KEY names itself. It applies to the KEYs of `get`,
`has`, `pop`, `delete` and `setdefault` and to the keys `delete -` reads. `delete`
names all its keys against the store as it was before the batch, guards the
tombstones that need it, turns stripping back on after a reset inside the batch,
and writes nothing (exit 1) when it must read the store to name a key and the read
is incomplete. tsvz-rs mirrors TSVZ.py's `_specStrippable`, `_specNameKey`,
`_specNamesKeys` and `_specDeleteRows` with `write::strippable`,
`write::name_key`, `ops::delete_needs_store` and `ops::delete_records`, used by
both the CLI and the engine; `Store::lookup_key` returns the named key.

## 3. Protocol (§21.4–§21.6, §21.10)

### 3.1 Requests

- A line is read up to `\n`, with one `\r` before it removed (§21.4.1), and
  split on TAB.
- Leading fields beginning with `--` are options. Known: `--sync` (§21.5.2) and
  `--x-written` (TSVZ.py's extension: acknowledge once written and report a
  failed write; section 4.3). Any other option gets status 2.
- The operation is one of §20.2's except `serve`, plus `stop` (§21.7.1). Its
  arguments are §13-decoded with TAB; `ops::check_args` validates them (status 2
  on failure, §21.10.1). A raw first field matching the reserved pattern (§12.2)
  is a marker key; one written `<#>…` is a data key (§21.4.3).
- `setdefault` with a marker key gets status 2 (§21.7.3).
- **Bulk input** (§21.7.2): after `set -`, `append -` or `delete -`, record lines
  in the protocol encoding are read up to a line that is exactly `#`. Nothing is
  parsed or applied before that line arrives; a connection that ends first
  writes nothing. The records are one batch.
- A line longer than 64 MiB (§21.10.2 asks for at least 1 MiB; TSVZ.py's limit)
  closes the connection.
- Invalid UTF-8 in a request is replaced with U+FFFD, as TSVZ.py does.

### 3.2 Responses

A response is written in one `write` after the request has been handled:

1. diagnostic lines `#!<TAB>TEXT`, TEXT §13-encoded: the warnings raised while
   handling this request (section 4.6);
2. output lines, in the TSVZ variant whatever the store's (§21.6.1): rows as
   `read --format records` prints them, `keys` one key per line, `len` the
   count, `verify` and `parts` their fields; a first field beginning with `#`
   is written `<#>…`;
3. the status line `#N`, or `#N<TAB>MESSAGE` with MESSAGE §13-encoded.

### 3.3 Statuses

Exit statuses are those of §20.6, in the cases TSVZ.py uses; messages are
tsvz-rs's own (§20.8.1 lets diagnostics differ).

| Case | Status |
|---|---|
| Done | 0 |
| A read operation (`read get has len keys pop popitem scrub verify parts`) on a missing store | 1, "no such store" |
| A read that skipped a part or stopped at damaged data | 1, with what was read (§21.8.5) |
| `pop`, `popitem`, `setdefault` on an incomplete read | 1, nothing written |
| `clear` or `scrub` refused | 1, nothing written |
| A write that failed, when the request waited for it | 1, the error |
| Usage error | 2 |
| Missing key (`get`, `has`, `pop`, `popitem` on an empty store) | 3 |
| Checksum mismatch | 4 |

A request never ends the handler: every failure becomes a status (§21.10.3).
Request handling must not panic, since the release build aborts on panic.

## 4. The engine (`engine.rs`)

### 4.1 State

One `Engine` per handler: a `Mutex` around an `Option<Store>` (Scope `All`,
`skip_unreadable` on; `None` while the store is missing), plus the writer of
section 4.2. Every request that reads takes the mutex, waits until every queued
write has been written (the writer's `barrier`), then calls `Store::refresh`
(core section 3.7; a missing store is opened again). The state is therefore what
a reader of the files returns at that moment, including other writers' appends,
new parts and in-place rewrites (§21.8.1, §21.8.5).

### 4.2 The writer thread

- `submit(payload, durable, member_size) -> seq` queues the bytes of one request,
  already encoded in the store's variant (`write::encode_record`).
- The thread takes everything queued and appends it as one batch with one
  `Writer::append_payload` call: one active-part lookup (§17.7, §21.8.4), one
  write lock (core section 4.3, shared with TSVZ.py), one `write`. When any
  entry of the batch is durable, the batch is fsync'd; otherwise the part is
  marked unsynced. A new batch starts as soon as the previous one ends, so
  concurrent producers share appends (§18.3).
- An entry is submitted as waited when its request will wait for it (durable,
  `--x-written`, `pop`, `popitem`, `setdefault`); the writer thread keeps the
  outcome of each waited entry, after the batch is written (and fsync'd when
  durable), until `wait(seq)` takes it: a failed write is never answered as
  written, however many batches fail after it. `barrier()` waits until
  everything submitted is written. `flush()` waits, then fsyncs every part a
  batch was appended to without an fsync since the last one, through the file it
  was appended through (still right after a `rename` rotation); the writer thread
  does the same as it ends, and as soon as a batch goes to another part (the part
  left behind is synced and its file closed, so open files never pile up).
- The thread never takes the engine mutex, so a request holding the mutex can
  wait on it.
- Whether an entry is durable (`#_write_ack_#`) and the member size used for a
  compressed active part (`#_member_size_#`) are the markers as they stand where
  the entry will be appended, as far as can be known when it is submitted: the
  markers the engine last followed, then the last values of those two markers
  queued since (they govern until the engine follows the files past them). The
  engine follows the files first when another writer may have written since:
  the writer thread counts batches that found their part not as the previous
  batch left it, and `submit` checks that the last part appended to is still as
  that batch left it and that the store has the same parts (by name; the
  directory is listed only while its timestamp cannot show a change).

### 4.3 Acknowledgement (§21.9)

| Request | Answered after |
|---|---|
| `set`, `append`, `delete` (one record or bulk) | queued (§21.9.1) |
| … while `#_write_ack_#` is `disk`, or with `--sync` | its batch is written and fsync'd (§21.9.2) |
| … with `--x-written` | its batch is written; a failed batch answers status 1 with the error |
| `pop`, `popitem` | their tombstone is written; on failure status 1 and no row |
| `setdefault` that writes | its record is written |
| `stop` | everything acknowledged is written and fsync'd (§21.9.3) |

A batch that fails after its writes were acknowledged is reported on `serve`'s
stderr, and every response carries a `#!` warning that writes are failing until
a batch succeeds again (TSVZ.py's behaviour).

### 4.4 Operations

- `read`, `get`, `has`, `len`, `keys`: under the mutex, after barrier and
  refresh, through `ops`. Status 1 when the store is incomplete.
- `set`, `append`, `delete`: records encoded and submitted under the mutex (so
  writes on one connection keep their order, §21.8.3), answered per section 4.3.
  A marker key writes a marker line; a key alone is a tombstone; `delete` of a
  marker key resets it. `set` and `append` write as the CLI does. A `delete`
  that must name a key (section 2.3) reads the store under the mutex, so that
  naming and writing are atomic (§21.8.2), and guards its tombstones (§9.6); when
  that read is incomplete it answers 1, "could not read every part of the store;
  nothing was written".
- `pop`, `popitem`, `setdefault`: under the mutex for both the read and the
  submit (§21.8.2), through `ops`.
- `clear`, `scrub`: under the mutex, barrier, then `maint::clear`/`maint::scrub`,
  then a full reload. Refused or failed: status 1.
- `verify`: barrier, then a verifying read of the files (`Scope::KeysOnly`,
  `verify` on, `skip_unreadable` on) through `ops::verify`.
- `parts`: barrier, then `ops::parts`.
- `stop`: handled by the server (section 6).

### 4.5 Creating the store

`serve` creates a missing store before serving: an empty unnumbered file, as
`set` creates one (§21.13.1, §20.2.2). A store removed while served is `None`
until a write creates it again; reads answer "no such store".

### 4.6 Diagnostics

The store's warning callback pushes into a buffer the engine drains per request,
under the mutex, so each request's `#!` lines are the warnings its own refresh,
read or write raised. Each warning also goes to `serve`'s stderr (unless `-q`); a failed
batch or fsync is an error and goes there even with `-q`, which prints errors only.

## 5. Pointer file and socket (§21.2, §21.3, §21.11)

### 5.1 Starting

`serve STORE`:

1. Refuses a loose store: exit 1, "serve takes a strict store (.tsvz .csvz .nsvz
   .psvz)".
2. Takes over the pointer file `STORE.serve` (the store path as given plus
   `.serve`, §21.2.1), as TSVZ.py does:
   - an existing pointer is opened with `O_RDONLY|O_NOFOLLOW|O_NONBLOCK`; if it
     is not a regular file, exit 1 and say to remove it;
   - `flock(LOCK_EX|LOCK_NB)` on it fails: another handler serves; exit 1,
     "already served by pid P on host H";
   - otherwise it is stale: it is unlinked, never written into;
   - the new pointer is created with `O_RDWR|O_CREAT|O_EXCL|O_NOFOLLOW`, mode
     0600, locked, and checked to be the file the path names (same inode);
     losing a race to another handler means busy.
3. Blocks SIGINT, SIGTERM and SIGHUP in every thread (section 6.2), then creates
   the store if it is missing and loads it.
4. Binds the socket at `DIR/s`, where DIR is a new `tsvz-XXXXXX` directory made
   by `mkdtemp`:
   - under `$XDG_RUNTIME_DIR` when it is an absolute, writable directory and neither
     `--x-group` nor `--x-mode` is given; otherwise under `$TMPDIR` made absolute, else
     `/tmp` (others cannot enter a private runtime directory; the published address must
     mean the same to every client);
   - the socket's mode is 0600 by default, 0660 with `--x-group G`, or
     `--x-mode M` (octal); with `--x-group` the directory and socket get group G;
   - DIR is 0700, plus group or other search permission when the socket's mode
     grants them access.
5. Writes the pointer: `tsvz-handler<TAB>1`, `address<TAB>unix:PATH`,
   `host<TAB>HOSTNAME` (`gethostname`), `pid<TAB>PID`, each value §13-encoded;
   mode 0644 (readable by all, writable by its owner, §21.2.3); fsync.
6. Logs "serving STORE at unix:PATH (pid N)" on stderr and answers requests.

### 5.2 Trust checks (client side, §21.2.4)

A client (the CLI's routing, `stop`, and `Client`) uses a pointer file only when:

- it is a regular file (opened with `O_NOFOLLOW|O_NONBLOCK`), at most 64 KiB;
- it is owned by the client's user or by the store's owner (the owner of the
  unnumbered file, else of the first part), and not writable by group or others;
- it has one name (link count 1): a hard link lets anyone place a file its owner wrote
  elsewhere;
- its first line is `tsvz-handler<TAB>1`, it has `address` and `host`, and no
  value has a control character;
- `host` is this host's name and `address` begins with `unix:`;
- the socket path is a socket owned by the pointer's owner;
- the process accepting the connection runs as the pointer's owner (`SO_PEERCRED`
  on Linux, `getpeereid` on macOS);
- the handler accepts the connection (retrying for up to 0.5 s while the listen
  queue is full).

A pointer without `address` or `host` is read once more after 50 ms, since a
handler may be writing it. Unknown names are ignored; `x-` names are
implementations' (§21.2.2).

## 6. Connections and stopping

### 6.1 Connections

The main thread accepts; each connection gets a thread that reads requests one at
a time (§21.3.2), reads a bulk request's lines up to `#`, has the engine handle
it and writes the response. A connection whose client goes away ends quietly.
The server tracks open connections for the idle timeout and for closing.

### 6.2 Stopping

- **`stop` request** (§21.7.1): `flush()`, answer `#0`, then stop.
- **Signals**: SIGINT, SIGTERM and SIGHUP stop the handler cleanly. They are
  blocked in every thread and received by one thread with `sigwait`. A signal
  that was ignored when `serve` started (`nohup`) stays ignored. During the
  close they stay blocked, so a second signal cannot cut the final writes short.
- **`--x-idle-timeout S`**: stop after S seconds with no open connection.
- **Close**: stop accepting (the stopper wakes the accept loop by connecting to
  the socket); shut down open connections; drain the writer and fsync; remove
  the socket directory; remove the pointer file if the path still names this
  handler's file (same inode); log "stopped serving STORE"; exit 0 (§21.13.1).

### 6.3 `stop STORE` (§21.13.2)

Finds and checks the pointer (section 5.2), connects, sends `stop` and waits for
its status line; then waits up to 10 s until the pointer no longer names that
pid. Exit 0 when the handler answered `#0`; exit 1, with the reason, when there
is no usable pointer or no handler answers.

## 7. The command line

### 7.1 New options and operations

- `serve` and `stop` take no ARG. `serve` runs in the foreground until stopped.
- `--x-direct`: work on the files even when a handler runs.
- `--x-idle-timeout S`, `--x-group G` (a name or number), `--x-mode M` (octal):
  for `serve`; bad values are usage errors (exit 2). Same spellings and meanings
  as TSVZ.py's, so scripts work with either tool.
- The help text describes `serve`, `stop` and the four options.

### 7.2 Routing (§20.8)

The CLI forwards a command line when all of these hold:

- the operation is not `serve`, `stop`, `verify` or `parts` (the last two read
  the files themselves and gain nothing from a handler; TSVZ.py routes them the
  same way);
- the store is strict, and `--x-direct` is not given;
- `STORE.serve` exists and passes the checks of section 5.2.

When no handler runs this costs one failed `open`. When a pointer exists but
cannot be used:

- an untrusted pointer: warn "ignoring the pointer file: WHY; using the files
  directly" (§20.8.2);
- a handler on another host, or one this user may not connect to: work on the
  files quietly;
- a handler that does not answer: warn and work on the files.

The request is the operation and each argument §13-encoded with TAB. Writes
(`set`, `append`, `delete`) carry `--x-written`, so a write that fails exits 1
as it does on the files. Bulk input is read from stdin by `ops::parse_records`
in the store's variant and sent as protocol lines (`ops::protocol_record`)
ended by `#`.

The response is printed as the operation prints on the files: `#!` lines go to
stderr (hidden by `-q`); rows and lines go through the same `Emitter`/`Fields`,
as records or a table; the status line's number is the exit status, and its
message is printed for status 1 and 2. The handler deletes the row a `pop` or `popitem`
takes before answering, whereas on the files the row is printed first: the
command prints and flushes that row with SIGPIPE ignored, and a row it cannot
print (a closed pipe, a full disk) is put back with `setdefault` (which keeps a
row written for its key since), reported on stderr, and the command exits 1. The
key goes back as a data key (§21.4.3); the row goes back at the end of the
order, and read as the store now strips (§10). When the store then reads
otherwise for that key, stderr shows the row taken as `read --format records`
prints it, ready for `set STORE -`.

If the connection is lost during the request, a read-only operation (`read`,
`get`, `has`, `len`, `keys`) warns and runs on the files; any other exits 1 with
"lost the handler; the OPERATION may or may not have been applied".

With `-v`, a routed command names the handler it used, and `serve -v` reports the
status of every request it answers.

## 8. The Rust client (`handler::Client`)

```rust
pub struct Client { /* one connection */ }

impl Client {
    /// Connect to the handler of `store` (section 5.2). Errors say why there is none.
    pub fn connect(store: impl AsRef<Path>) -> Result<Client, Error>;
    pub fn sync(self, on: bool) -> Client;                    // --sync on writes
    pub fn timeout(self, limit: Option<Duration>) -> Client;  // per answer
    pub fn warnings(self, warn: Option<WarnFn>) -> Client;    // receives #! lines

    pub fn read(&mut self) -> Result<Vec<Vec<String>>, Error>;
    pub fn get(&mut self, key: &str) -> Result<Option<Vec<String>>, Error>;  // Some for a live key only
    pub fn has(&mut self, key: &str) -> Result<bool, Error>;
    pub fn len(&mut self) -> Result<usize, Error>;
    pub fn keys(&mut self) -> Result<Vec<String>, Error>;
    pub fn set(&mut self, records: &[Record]) -> Result<(), Error>;          // one batch
    pub fn delete(&mut self, keys: &[&str]) -> Result<(), Error>;
    pub fn pop(&mut self, key: &str) -> Result<Option<Vec<String>>, Error>;
    pub fn popitem(&mut self, last: bool) -> Result<Option<Vec<String>>, Error>;
    pub fn setdefault(&mut self, key: &str, values: &[&str]) -> Result<Vec<String>, Error>;
    pub fn clear(&mut self) -> Result<(), Error>;
    pub fn scrub(&mut self) -> Result<(), Error>;
    pub fn verify(&mut self) -> Result<Vec<Vec<String>>, Error>;           // mismatch fields
    pub fn stop(self) -> Result<(), Error>;

    /// The raw form: fields are sent §13-encoded; `bulk` lines follow, ended by `#`.
    pub fn request(&mut self, fields: &[&str], bulk: Option<&[Record]>) -> Result<Response, Error>;
}

pub struct Response { pub status: i32, pub lines: Vec<String>, pub diagnostics: Vec<String>, pub message: String }
```

- Rows are decoded: key first, then cells. `set` sends one record as `set …`
  and several as one `set -` batch. Writes carry `--x-written`.
- Status 0, 3 and 4 are answers. Status 1 and 2 become
  `Error::Handler { status, message }`.
- `connect` fails with `Error::NoHandler(reason)`. A connection that breaks
  during a request is `Error::HandlerLost(reason)`, which says the request may or
  may not have been applied; the client does not reconnect or fall back.
- `request` gives what the typed calls hide: the defaults row of a missing key,
  partial rows with status 1, `parts`.

## 9. Interoperability with TSVZ.py

- The protocol is the spec's, plus `--x-written`, which both tools accept and
  both CLIs send with writes.
- Pointer files have the same format and the same `flock`: a TSVZ.py `serve`
  refuses while a tsvz-rs handler holds the pointer, and the reverse; either
  tool's `stop` stops either handler.
- The handler appends under the write lock shared with TSVZ.py (core section
  4.3), so it, TSVZ.py writers and tsvz-rs writers exclude each other, and it
  follows their appends (§21.1.2).
- TSVZ.py's handler writes `x-delimiter` for loose files; tsvz-rs never forwards
  loose files, so it ignores that name.
- Where the spec is silent, the choices are TSVZ.py's: the socket location and
  modes, the 64 MiB line limit, `verify` and `parts` not routed, the status
  cases of section 3.3, acknowledging `pop` after its tombstone is written, the
  failing-writes warning, `stop` waiting for the pointer to go. Each is a row in
  `docs/silent-choices.md`, with the two choices that differ: tsvz-rs serves
  strict stores only, and the skipped-part behaviour of section 2.1 (which
  matches TSVZ.py's handler and now its CLI too).

## 10. Testing

- **Unit tests**: request parsing (options, markers vs `<#>` keys, `\r`, empty
  operation, arity via `ops::check_args`), response encoding (`<#>`, `#!`,
  status lines); pointer take-over (busy, stale, symlink, not a regular file,
  inode race) and every trust rule; the writer thread (batching, `wait`,
  `barrier`, durable entries fsync'd, a failed batch reported to its waiters and
  as failing); the skipped-part store (warning, later parts read, `incomplete`,
  refresh).
- **`tests/handler.rs`**, driving a real `tsvz serve` through `Client` and raw
  sockets: read-your-writes; direct appends and a new part are seen; an in-place
  rewrite followed by appends past the old size is seen (the core design's
  follow-up); concurrent `pop`s of one key: exactly one wins; a cut-off bulk
  request writes nothing; `disk` and `--sync` writes are in the file when
  answered; writes are in the file after `stop`; SIGTERM and SIGINT stop
  cleanly and remove the pointer and socket; the idle timeout; a second `serve`
  is refused; routing: output and exit status equal the files', `--x-direct`,
  an untrusted pointer warns and uses the files, a handler killed mid-request
  (read falls back, write exits 1); an unreadable part: status 1 with what was
  read, on the files and through the handler.
- **Interop** (`tests/interop.rs`; skipped without TSVZ.py, required in CI): the
  tsvz-rs CLI routed through a TSVZ.py handler; TSVZ.py's CLI and `TSVZClient`
  against a tsvz-rs handler; each tool's `stop` stops the other's handler; each
  tool's `serve` is refused while the other serves.
- **Conformance**: the `handler` class (37 cases) and the socket and forwarded
  runs of the reader, writer and cli cases pass; the command becomes
  `tsvzconform.py --impl target/release/tsvz --class reader,writer,cli,handler
  --feature bzip2,xz,zstd,crc32,crc32c,sha256,blake3,file-first,handler`.
- **difftest**: `--handler` starts a tsvz-rs handler per seed and sends
  tsvz-rs's operations through it (routing), still compared byte for byte with
  TSVZ.py on the files.
- **Fuzzing**: a `request` target feeding arbitrary bytes to the request parser
  and the bulk reader; it must not panic.

## 11. Performance

- tsvzbench `perop` (its `cli via handler` rows) and `socket` workloads, TSVZ.py
  as the baseline, work directory on `/mnt/gen5`; recorded in
  `bench/RESULTS.md` with the other families. The bar: faster than TSVZ.py on
  every handler workload, as on the others.
- Where the speed comes from: no interpreter start-up for a routed command; a
  refresh that only stats the parts when nothing changed; writes encoded once and
  appended in batches; responses written in one `write`.
- If a workload misses the bar: profile it before release. Candidates: keeping
  the active part open between batches, and skipping the directory listing when
  its mtime is unchanged and not racy (already so in `refresh`).

## 12. Docs, CI and release

- README: a handler section (serve, stop, routing, `--x-direct`, the client,
  access and `--x-group`/`--x-mode`).
- `docs/silent-choices.md`: the rows of section 9.
- CI: the conformance command of section 10, and difftest with and without
  `--handler`, on Linux and macOS.
- Release 0.1.0 with the handler, after a final whole-branch review: the core
  plan's Task 23 (create the GitHub repository and push, `cargo publish` behind
  the release gate, tag `v0.1.0` so the release workflow attaches the four
  binaries). Each outward step waits for the user's explicit yes.

## 13. Done

- Conformance reports reader, writer, cli and handler ✓, no FAIL and no warn
  beyond those listed in `docs/silent-choices.md`, on Linux and macOS.
- difftest: no unexplained difference over its CI seed range, with and without
  `--handler`.
- `bench/RESULTS.md` shows tsvz-rs faster than TSVZ.py on every workload,
  handler ones included.
- Unit, handler, interop and fuzz tests pass; `docs/silent-choices.md` and the
  README cover the handler.
- 0.1.0 is on crates.io and the GitHub release carries the four binaries.

## Follow-ups outside this sub-project

- A TSVZ conformance case for an in-place rewrite followed by appends past the
  old size, so every handler is held to it (TSVZ repository).
- A Rust row in tsvz.org's implementations table and a tsvz-rs row in
  `cas/CLAUDE.md`, after the release.
- Sub-project 3: the multi-part scrub (§19.2).
