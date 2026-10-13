# tsvz-rs write handler Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** tsvz-rs serves stores itself (`tsvz serve`, `tsvz stop`), routes ordinary commands through a running handler of either tool (§20.8), gives Rust programs a `handler::Client`, and passes the TSVZ conformance suite's handler class; then the first public release, 0.1.0, ships core and handler together.

**Architecture:** A shared `ops` layer computes each §20.2 operation once, for the command line on the files and for the handler, so a handler answers what the files give. The handler (`src/handler/`) is std threads with no new dependencies: one engine holds the store behind a mutex and follows the files through `Store::refresh`; one writer thread appends every queued write as one batch; one thread per connection speaks the line protocol; the pointer file announces the handler and clients check it before trusting it. The CLI forwards a command line when a trusted handler runs.

**Tech Stack:** Rust 2024 (stable, MSRV 1.85); std (`std::os::unix::net`), `libc` (`flock`, peer credentials, `gethostname`, signals, `mkdtemp`, `getgrnam`); `tempfile` for tests; Python 3 for the conformance suite, difftest, interop tests and benchmarks; nightly + cargo-fuzz for the fuzz target.

**Spec:** `docs/superpowers/specs/2026-10-12-tsvz-rs-handler-design.md` (this repo). The format spec is `../TSVZ/tsvz-spec-v1.md` (§20.8, §21); the reference implementation `../TSVZ/TSVZ.py` (4.6: `_ServeStore`, `_ServeWriter`, `_ServeHandle`, `_serveLookup`, `_cliHandler`); the conformance runner `../TSVZ/tsvzconform.py`; the benchmark `../TSVZ/tsvzbench.py`. The core design (`2026-10-12-tsvz-rs-core-design.md`) and plan describe the crate this builds on.

## Global Constraints

- Repository `cas/tsvz-rs` (this directory), work on branch `handler` cut from `main`; the TSVZ repository is checked out beside it at `../TSVZ`.
- Crate `tsvz` stays at version `0.1.0` (the first public release ships core and handler together), `edition = "2024"`, `rust-version = "1.85"`, `license = "GPL-3.0-or-later"`.
- No new dependency: std plus `libc`, which the crate already uses.
- Platforms: Linux and macOS. Unix-only APIs are fine; peer credentials use `SO_PEERCRED` on Linux and `getpeereid` on macOS.
- The handler serves strict stores only (`.tsvz .csvz .nsvz .psvz`); the CLI never forwards a loose file.
- The protocol is TAB-delimited and §13-encoded whatever the store's variant (§21.4.2); the handler accepts TSVZ.py's `--x-written` option.
- No panic on any request, pointer file, socket or peer: failures become statuses, `Error`s or warnings. The release build aborts on panic, which would kill the handler.
- Where `../TSVZ/tsvz-spec-v1.md` is silent, behave as `../TSVZ/TSVZ.py` does and add the choice to `docs/silent-choices.md`.
- Benchmarks run with a work directory on `/mnt/gen5` (f2fs), never `/tmp` (tmpfs) or `/mnt/klein` (mergerfs).
- Every commit message ends with these two lines:
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`
  `Claude-Session: https://claude.ai/code/session_0198xw4TRnucPbiyi9csx4uj`
- Outward-facing steps (creating the GitHub repository, pushing, `cargo publish`, tagging) wait for the user's explicit yes, each time. `cargo publish` is also blocked by the release-gate hook until a review and `release_gate.py approve`.

## Review Focus

Conditions the spec implies but the conformance suite does not exercise, most likely to bite first. Each has a test in the task named.

1. **A client that hangs up before reading its answer** — the handler keeps serving everyone else; a write to a closed socket must not end the process (test in Task 7: `a_client_that_hangs_up_before_its_answer_leaves_the_handler_serving`).
2. **A connection left half-sent when the handler stops** — `stop` still ends promptly and closes it, rather than waiting on a line that never comes (test in Task 7: `stop_ends_the_connections_still_open`).
3. **A socket path too long to bind** (a deep `$TMPDIR` or `$XDG_RUNTIME_DIR`) — `serve` exits 1 with a message and leaves no pointer file and no socket directory behind (test in Task 7: `a_socket_path_too_long_to_bind_fails_cleanly`).
4. **Many clients at once** — 32 connections each writing and reading back 50 keys all get their own answers and lose nothing (test in Task 8: `many_clients_at_once_all_get_their_answers`).
5. **Another tool rewrites the active part in place, then appends past its old size** — the handler reads the store again instead of following stale offsets (test in Task 5: `a_part_rewritten_in_place_then_grown_past_its_old_size_is_read_again`).

---

## How to apply this plan's code

Every block of code in a step is exact, and is introduced by one of these phrases:

- **Create `PATH`:** — write the block as the whole new file.
- **Create `PATH` holding only its test module:** — a new file whose implementation comes in a later step.
- **Put this above the test module in `PATH`:** — insert the block at the top of the file, followed by one empty line, before `#[cfg(test)]`.
- **Replace `PATH` with:** — overwrite the whole file.
- **Apply:** — a unified diff against the repository root: save the block to a file and run `git apply FILE` (or make the same edit by hand).

Run `cargo fmt` after applying a step; the code is already formatted, so it should change nothing.

## File Structure

```
src/store.rs              Options::skip_unreadable, unreadable/incomplete, refresh stamps; lookup_key (Task 1, 3)
src/maint.rs              rewrites never read past an unreadable part                              (Task 1)
src/ops.rs                the §20.2 operations, shared by the CLI and the engine; Out, Plan        (Task 2, 3)
src/write.rs              strippable, resolve, name_key (§20.2.7)                                  (Task 3)
src/handler/mod.rs        the module; re-exports Client, Response                                  (Task 4-8)
src/handler/protocol.rs   request parsing, responses, line reading (§21.4-§21.6, §21.10)           (Task 4)
src/handler/engine.rs     Engine: the live store, the batching writer thread (§21.7-§21.9)         (Task 5)
src/handler/pointer.rs    pointer files: take, publish, release, lookup, connect (§21.2)           (Task 6)
src/handler/server.rs     serve and stop: socket, connections, signals, idle timeout (§21.13)      (Task 7)
src/handler/client.rs     the public Client                                                        (Task 8)
src/error.rs              Error::NoHandler, Handler, HandlerLost                                   (Task 8)
src/cli/output.rs         Printer: ops::Out for records and tables                                 (Task 2)
src/cli/read.rs, write.rs the operations on the files, through ops                                 (Task 1-3)
src/cli/args.rs           ops::check_args; --x-direct, --x-idle-timeout, --x-group, --x-mode       (Task 2, 7)
src/cli/serve.rs          serve and stop from the command line                                     (Task 7)
src/cli/route.rs          forwarding a command line to a handler (§20.8)                           (Task 9)
src/cli/mod.rs            dispatch: route first, then the files                                    (Task 1, 7, 9)
tests/cli.rs              unreadable parts, naming a key; the obsolete serve test goes             (Task 1, 3, 7)
tests/handler.rs          a real `tsvz serve`, driven over its socket and the CLI                  (Task 7-9)
tests/interop.rs          handlers and commands of both tools together                             (Task 10)
tests/robust.rs           random requests never panic                                              (Task 11)
fuzz/fuzz_targets/request.rs, fuzz/Cargo.toml                                                      (Task 11)
difftest/difftest.py      --handler                                                                (Task 11)
README.md, docs/silent-choices.md, src/lib.rs, .github/workflows/ci.yml                            (Task 12)
bench/RESULTS.md          the handler workloads                                                    (Task 13)
```

---

### Task 1: Skip parts that cannot be read; refresh after an early stop

The handler must answer status 1 with what it could read when a part cannot be read (§21.8.5); a forwarded command must print what the files give (§20.8.1), so the CLI does the same on the files. A `Store` learns to skip such a part when asked. `clear` and `scrub` never skip: they would rewrite the store without that part's rows. A second fix comes with it: a read that a `stop` policy ended before the last part left that part unstamped, and `Store::refresh` then panicked on it.

**Files:**
- Modify: `src/store.rs`, `src/maint.rs`, `src/cli/mod.rs`, `src/cli/read.rs`, `src/cli/write.rs`
- Test: `src/store.rs`, `src/maint.rs` (unit tests), `tests/cli.rs`

**Interfaces:**
- Consumes: `Store`, `Options`, `parts::Part`, `parts::Stamp` (core).
- Produces: `Options::skip_unreadable: bool` (default false); `Store::unreadable(&self) -> &[PathBuf]`; `Store::incomplete(&self) -> bool` (a part skipped, or the read stopped); `Store::refresh` safe after a stop before the last part. `maint::clear`/`scrub` always read with `skip_unreadable: false`. The CLI opens stores with `skip_unreadable: true` and exits 1 when `incomplete()`.

- [ ] **Step 1: Create the branch and write the failing tests**

```bash
git checkout -b handler main
```

Apply:

```diff
--- a/src/store.rs
+++ b/src/store.rs
@@ -953,4 +1017,83 @@ mod tests {
         assert_eq!(s.refresh().unwrap(), Refreshed::Followed);
         assert_eq!(s.header(), Some("#hdr"));
     }
+
+    fn skipping(warned: &Arc<std::sync::Mutex<Vec<crate::Warning>>>) -> Options {
+        let w = warned.clone();
+        Options {
+            skip_unreadable: true,
+            warnings: Some(Arc::new(move |x: &crate::Warning| {
+                w.lock().unwrap().push(x.clone())
+            })),
+            ..Options::default()
+        }
+    }
+
+    #[test]
+    fn an_unreadable_part_is_skipped_only_when_asked() {
+        let dir = tempfile::tempdir().unwrap();
+        let p = write(dir.path(), "s.tsvz", b"#_defaults_#\td1\td2\na\t1\n");
+        let hole = dir.path().join("s.tsvz.1");
+        fs::create_dir(&hole).unwrap();
+        write(dir.path(), "s.tsvz.2", b"b\t2\n");
+        assert!(matches!(
+            Store::open(&p, Options::default()),
+            Err(Error::Io { .. })
+        ));
+        let warned = Arc::new(std::sync::Mutex::new(Vec::new()));
+        let s = Store::open(&p, skipping(&warned)).unwrap();
+        // The replay goes on past the skipped part with the markers it had reached.
+        assert_eq!(rows(&s), vec![v(&["a", "1", "d2"]), v(&["b", "2", "d2"])]);
+        assert_eq!(s.unreadable(), std::slice::from_ref(&hole));
+        assert!(s.incomplete() && s.stopped().is_none());
+        let warned = warned.lock().unwrap();
+        assert_eq!(warned.len(), 1);
+        assert_eq!(warned[0].part, hole);
+        assert!(warned[0].message.contains("skipped"), "{:?}", warned[0]);
+        let whole = write(dir.path(), "t.tsvz", b"a\t1\n");
+        assert!(
+            !Store::open(whole, skipping(&warned_none()))
+                .unwrap()
+                .incomplete()
+        );
+    }
+
+    fn warned_none() -> Arc<std::sync::Mutex<Vec<crate::Warning>>> {
+        Arc::new(std::sync::Mutex::new(Vec::new()))
+    }
+
+    #[test]
+    fn a_store_with_a_skipped_part_follows_appends_and_reloads_when_it_changes() {
+        let dir = tempfile::tempdir().unwrap();
+        let p = write(dir.path(), "s.tsvz", b"a\t1\n");
+        let hole = dir.path().join("s.tsvz.1");
+        fs::create_dir(&hole).unwrap();
+        let last = write(dir.path(), "s.tsvz.2", b"b\t2\n");
+        let mut s = Store::open(&p, skipping(&warned_none())).unwrap();
+        assert_eq!(s.refresh().unwrap(), Refreshed::Unchanged);
+        append(&last, b"c\t3\n");
+        assert_eq!(s.refresh().unwrap(), Refreshed::Followed);
+        assert_eq!(keys(&s), vec!["a", "b", "c"]);
+        assert!(s.incomplete());
+        fs::remove_dir(&hole).unwrap();
+        fs::write(&hole, "x\t9\n").unwrap();
+        assert_eq!(s.refresh().unwrap(), Refreshed::Reloaded);
+        assert_eq!(keys(&s), vec!["a", "x", "b", "c"]);
+        assert!(s.unreadable().is_empty() && !s.incomplete());
+    }
+
+    #[test]
+    fn a_skipped_last_part_reloads_on_any_change() {
+        let dir = tempfile::tempdir().unwrap();
+        let p = write(dir.path(), "s.tsvz", b"a\t1\n");
+        let hole = dir.path().join("s.tsvz.1");
+        fs::create_dir(&hole).unwrap();
+        let mut s = Store::open(&p, skipping(&warned_none())).unwrap();
+        assert_eq!(s.refresh().unwrap(), Refreshed::Unchanged);
+        fs::remove_dir(&hole).unwrap();
+        fs::write(&hole, "b\t2\n").unwrap();
+        assert_eq!(s.refresh().unwrap(), Refreshed::Reloaded);
+        assert_eq!(keys(&s), vec!["a", "b"]);
+        assert!(!s.incomplete());
+    }
 }
```

Apply:

```diff
--- a/src/maint.rs
+++ b/src/maint.rs
@@ -721,4 +724,21 @@ mod tests {
             "backup mode {mode:o} has setuid, setgid, sticky, execute or shared write bits"
         );
     }
+
+    #[test]
+    fn clear_never_reads_past_an_unreadable_part() {
+        // Tombstones for the keys a partial read saw would miss the unread part's keys.
+        let dir = tempfile::tempdir().unwrap();
+        let p = dir.path().join("s.tsvz");
+        fs::write(&p, "a\t1\n").unwrap();
+        fs::create_dir(dir.path().join("s.tsvz.1")).unwrap();
+        let last = dir.path().join("s.tsvz.2");
+        fs::write(&last, "b\t2\n").unwrap();
+        let skipping = Options {
+            skip_unreadable: true,
+            ..opts()
+        };
+        assert!(matches!(clear(&p, &skipping), Err(Error::Io { .. })));
+        assert_eq!(fs::read_to_string(&last).unwrap(), "b\t2\n");
+    }
 }
```

Apply (the old non-root test now expects what was read; the new test uses a directory as a part, which even root cannot read as a file):

```diff
--- a/tests/cli.rs
+++ b/tests/cli.rs
@@ -140,6 +140,35 @@ fn missing_and_unreadable_stores_exit_1_naming_the_path() {
     assert!(code == 1 && err.contains("not a TSVZ store path"), "{err}");
 }
 
+#[test]
+fn a_part_that_cannot_be_read_is_skipped_and_the_read_fails() {
+    // §21.8.5 and §4.10: print what could be read, exit 1; change nothing on such a read.
+    let d = store("a\t1\n");
+    fs::create_dir(d.path().join("s.tsvz.1")).unwrap();
+    let last = d.path().join("s.tsvz.2");
+    fs::write(&last, "b\t2\n").unwrap();
+    let (code, out, err) = records(d.path(), &["read", "s.tsvz"]);
+    assert_eq!((code, out.as_str()), (1, "a\t1\nb\t2\n"));
+    assert!(err.contains("s.tsvz.1") && err.contains("skipped"), "{err}");
+    assert_eq!(
+        records(d.path(), &["get", "s.tsvz", "b"]),
+        (1, "b\t2\n".into(), err.clone())
+    );
+    assert_eq!(records(d.path(), &["len", "s.tsvz"]).1, "2\n");
+    assert_eq!(records(d.path(), &["len", "s.tsvz"]).0, 1);
+    assert_eq!(records(d.path(), &["has", "s.tsvz", "b"]).0, 1);
+    for change in [
+        &["pop", "s.tsvz", "b"][..],
+        &["popitem", "s.tsvz"],
+        &["setdefault", "s.tsvz", "c", "3"],
+        &["clear", "s.tsvz"],
+    ] {
+        let (code, out, _) = records(d.path(), change);
+        assert_eq!((code, out.as_str()), (1, ""), "{change:?}");
+        assert_eq!(fs::read_to_string(&last).unwrap(), "b\t2\n", "{change:?}");
+    }
+}
+
 #[test]
 fn awkward_paths() {
     let d = tempfile::tempdir().unwrap();
@@ -671,7 +700,7 @@ fn an_unreadable_part_exits_1_naming_it() {
     fs::set_permissions(&part, fs::Permissions::from_mode(0o000)).unwrap();
     let (code, out, err) = records(d.path(), &["read", "s.tsvz"]);
     fs::set_permissions(&part, fs::Permissions::from_mode(0o600)).unwrap();
-    assert_eq!((code, out.as_str()), (1, ""), "{err}");
+    assert_eq!((code, out.as_str()), (1, "a\tx\n"), "{err}");
     assert!(
         err.contains("s.tsvz.1") && err.contains("ermission") && !err.contains("panicked"),
         "{err}"
```

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test --all-features --lib store:: 2>&1 | grep -E '^error' | head -3`
Expected: compile errors: `struct store::Options has no field named skip_unreadable`, `no method named unreadable`, `no method named incomplete`.

- [ ] **Step 3: Implement skipping**

`load` moves the per-part read into `read_part`; a failure there either returns the error or, when skipping, calls `skip`, which stamps the part, warns, and records it. `follow_append` reloads when the last part was skipped.

Apply:

```diff
--- a/src/store.rs
+++ b/src/store.rs
@@ -14,7 +14,7 @@ use crate::frame::Framer;
 use crate::name::{self, Codec, Format};
 use crate::parts::{self, Stamp};
 use crate::replay::{Binding, Cells, Damage, EMPTY_CELLS, Markers, Mismatch, Policy, Replay, Sink};
-use crate::{Error, WarnFn};
+use crate::{Error, WarnFn, Warning};
 
 /// What a store keeps of each row.
 #[derive(Clone, Debug, Default, PartialEq, Eq)]
@@ -37,6 +37,10 @@ pub struct Options {
     /// The delimiter of a loose file (`-d`); ignored for strict variants (§5.1).
     pub delimiter: Option<u8>,
     pub warnings: Option<WarnFn>,
+    /// Skip a part that cannot be opened or read, or whose codec this build lacks (§16.5),
+    /// with a warning, instead of failing: the store then holds what could be read, and
+    /// [`Store::incomplete`] is true (§21.8.5). Off by default.
+    pub skip_unreadable: bool,
 }
 
 /// One part as `parts` lists it (§20.2).
@@ -258,6 +262,13 @@ pub(crate) fn racy(mtime: i128) -> bool {
     now - mtime < RACY_NS
 }
 
+fn unsupported(part: &parts::Part) -> Error {
+    Error::UnsupportedCodec {
+        path: part.path.clone(),
+        codec: part.codec.map_or("", Codec::name),
+    }
+}
+
 fn read_check(file: &File, offset: u64) -> Vec<u8> {
     let start = offset.saturating_sub(CHECK_BYTES);
     let mut buf = vec![0u8; (offset - start) as usize];
@@ -275,6 +286,7 @@ pub struct Store {
     table: Table,
     replay: Replay,
     parts: Vec<PartInfo>,
+    unreadable: Vec<PathBuf>,
     pub(crate) follow: Follow,
 }
 
@@ -293,6 +305,7 @@ impl Store {
             format,
             options,
             parts: Vec::new(),
+            unreadable: Vec::new(),
             follow: Follow::default(),
         };
         store.load()?;
@@ -304,14 +317,13 @@ impl Store {
         if list.is_empty() {
             return Err(Error::Missing(self.path.clone()));
         }
-        if let Some(part) = list
-            .iter()
-            .find(|p| p.codec.is_some_and(|c| !c.supported()))
-        {
-            return Err(Error::UnsupportedCodec {
-                path: part.path.clone(),
-                codec: part.codec.map_or("", Codec::name),
-            });
+        if !self.options.skip_unreadable {
+            if let Some(part) = list
+                .iter()
+                .find(|p| p.codec.is_some_and(|c| !c.supported()))
+            {
+                return Err(unsupported(part));
+            }
         }
         let dir = dir_mtime(&self.path);
         self.follow = Follow {
@@ -322,64 +334,15 @@ impl Store {
         let last = list.len() - 1;
         for (i, part) in list.iter().enumerate() {
             self.replay.begin_part(&part.path, i == 0);
-            let mut file = File::open(&part.path).map_err(|e| Error::io(&part.path, e))?;
-            let meta = file.metadata().map_err(|e| Error::io(&part.path, e))?;
-            if meta.is_dir() {
-                return Err(Error::io(
-                    &part.path,
-                    std::io::Error::from(std::io::ErrorKind::IsADirectory),
-                ));
-            }
-            self.follow.stamps.push(Some(Stamp::of(&meta)));
-            let (replay, table) = (&mut self.replay, &mut self.table);
-            let mut framer = Framer::new();
-            match part.codec {
-                Some(codec) => {
-                    let mut data = Vec::new();
-                    file.read_to_end(&mut data)
-                        .map_err(|e| Error::io(&part.path, e))?;
-                    codec::for_each_item(codec, &data, &mut |item| match item {
-                        Item::Data(out) => framer.push(&out, &mut |l| replay.line(l, table)),
-                        Item::CutShort { offset, out } => {
-                            replay.cut_short(offset);
-                            framer.push(&out, &mut |l| replay.line(l, table))
-                        }
-                        Item::Corrupt { offset, out } => match replay.corrupt(offset, out.len()) {
-                            Policy::Recover => framer.push(&out, &mut |l| replay.line(l, table)),
-                            Policy::Skip => true,
-                            Policy::Stop => false,
-                        },
-                        Item::Break => {
-                            framer.brk();
-                            true
-                        }
-                    });
-                }
-                None => {
-                    let mut buf = vec![0u8; 1 << 20];
-                    loop {
-                        let n = match file.read(&mut buf) {
-                            Ok(0) => break,
-                            Ok(n) => n,
-                            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
-                            Err(e) => return Err(Error::io(&part.path, e)),
-                        };
-                        if !framer.push(&buf[..n], &mut |l| replay.line(l, table)) {
-                            break;
-                        }
-                    }
+            if let Err(e) = self.read_part(part, i == last) {
+                if !self.options.skip_unreadable {
+                    return Err(e);
                 }
+                self.skip(i, part, &e);
             }
             if self.replay.stopped.is_some() {
                 break; // a stop policy ends the read of the whole store (§4.10)
             }
-            self.replay.tail(framer.tail().len());
-            if i == last && part.codec.is_none() {
-                self.follow.offset = framer.committed();
-                self.follow.line = framer.number();
-                self.follow.tail = framer.tail().to_vec();
-                self.follow.check = read_check(&file, framer.committed());
-            }
         }
         self.parts = list
             .into_iter()
@@ -395,6 +358,95 @@ impl Store {
         Ok(())
     }
 
+    /// Replay one part; `last` marks the store's last part, whose end the follow state records.
+    fn read_part(&mut self, part: &parts::Part, last: bool) -> Result<(), Error> {
+        let mut file = File::open(&part.path).map_err(|e| Error::io(&part.path, e))?;
+        let meta = file.metadata().map_err(|e| Error::io(&part.path, e))?;
+        if meta.is_dir() {
+            return Err(Error::io(
+                &part.path,
+                std::io::Error::from(std::io::ErrorKind::IsADirectory),
+            ));
+        }
+        self.follow.stamps.push(Some(Stamp::of(&meta)));
+        if part.codec.is_some_and(|c| !c.supported()) {
+            return Err(unsupported(part));
+        }
+        let (replay, table) = (&mut self.replay, &mut self.table);
+        let mut framer = Framer::new();
+        match part.codec {
+            Some(codec) => {
+                let mut data = Vec::new();
+                file.read_to_end(&mut data)
+                    .map_err(|e| Error::io(&part.path, e))?;
+                codec::for_each_item(codec, &data, &mut |item| match item {
+                    Item::Data(out) => framer.push(&out, &mut |l| replay.line(l, table)),
+                    Item::CutShort { offset, out } => {
+                        replay.cut_short(offset);
+                        framer.push(&out, &mut |l| replay.line(l, table))
+                    }
+                    Item::Corrupt { offset, out } => match replay.corrupt(offset, out.len()) {
+                        Policy::Recover => framer.push(&out, &mut |l| replay.line(l, table)),
+                        Policy::Skip => true,
+                        Policy::Stop => false,
+                    },
+                    Item::Break => {
+                        framer.brk();
+                        true
+                    }
+                });
+            }
+            None => {
+                let mut buf = vec![0u8; 1 << 20];
+                loop {
+                    let n = match file.read(&mut buf) {
+                        Ok(0) => break,
+                        Ok(n) => n,
+                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
+                        Err(e) => return Err(Error::io(&part.path, e)),
+                    };
+                    if !framer.push(&buf[..n], &mut |l| replay.line(l, table)) {
+                        break;
+                    }
+                }
+            }
+        }
+        if self.replay.stopped.is_some() {
+            return Ok(());
+        }
+        self.replay.tail(framer.tail().len());
+        if last && part.codec.is_none() {
+            self.follow.offset = framer.committed();
+            self.follow.line = framer.number();
+            self.follow.tail = framer.tail().to_vec();
+            self.follow.check = read_check(&file, framer.committed());
+        }
+        Ok(())
+    }
+
+    /// Record part `i` as skipped: it could not be read (§21.8.5). The replay goes on with the
+    /// next part from the state it had reached.
+    fn skip(&mut self, i: usize, part: &parts::Part, e: &Error) {
+        if self.follow.stamps.len() == i {
+            let meta = std::fs::metadata(&part.path).ok();
+            self.follow.stamps.push(meta.map(|m| Stamp::of(&m)));
+        }
+        let reason = match e {
+            Error::Io { source, .. } => source.to_string(),
+            other => other.to_string(),
+        };
+        if let Some(w) = &self.options.warnings {
+            w(&Warning {
+                part: part.path.clone(),
+                offset: None,
+                message: format!(
+                    "skipped this part, which cannot be read ({reason}); the read is incomplete"
+                ),
+            });
+        }
+        self.unreadable.push(part.path.clone());
+    }
+
     pub fn path(&self) -> &Path {
         &self.path
     }
@@ -482,6 +534,17 @@ impl Store {
         self.replay.stopped.as_ref()
     }
 
+    /// The parts skipped because they could not be read (with [`Options::skip_unreadable`]).
+    pub fn unreadable(&self) -> &[PathBuf] {
+        &self.unreadable
+    }
+
+    /// Whether the read missed something: a part was skipped, or a `stop` policy ended it
+    /// (§4.10). Such a read has failed, and nothing should be changed on its strength.
+    pub fn incomplete(&self) -> bool {
+        self.replay.stopped.is_some() || !self.unreadable.is_empty()
+    }
+
     pub fn damaged(&self) -> bool {
         self.replay.damaged
     }
@@ -553,6 +616,7 @@ impl Store {
         let want = offset.min(CHECK_BYTES) as usize;
         if self.replay.stopped.is_some()
             || self.parts[last].codec.is_some()
+            || self.unreadable.contains(&path)
             || stamp.ino != old.ino
             || stamp.size < offset
             || stamp.size <= old.size
```

Apply:

```diff
--- a/src/maint.rs
+++ b/src/maint.rs
@@ -334,10 +334,13 @@ fn refuse_a_part(op: &str, path: &Path) -> Result<(), Error> {
     Ok(())
 }
 
+/// A whole read for a rewrite: never past a part it cannot read, since what it writes would
+/// lose that part's rows.
 fn whole(options: &Options) -> Options {
     Options {
         scope: Scope::All,
         verify: false,
+        skip_unreadable: false,
         ..options.clone()
     }
 }
```

Apply:

```diff
--- a/src/cli/mod.rs
+++ b/src/cli/mod.rs
@@ -167,6 +167,8 @@ impl Ctx {
             verify,
             delimiter: self.args.delimiter,
             warnings: Some(self.warn.clone()),
+            // Print what can be read and exit 1, as a handler answers (§21.8.5, §20.8.1).
+            skip_unreadable: true,
         }
     }
 
```

Apply:

```diff
--- a/src/cli/read.rs
+++ b/src/cli/read.rs
@@ -6,9 +6,9 @@ use crate::cli::Ctx;
 use crate::cli::output::{Emitter, Fields, stdout_error};
 use crate::{Error, Scope, Store, parts};
 
-/// A read that stopped at damaged data fails (§4.10, §20.6).
+/// A read that skipped a part or stopped at damaged data fails (§4.10, §20.6, §21.8.5).
 fn status(store: &Store, code: i32) -> i32 {
-    if store.stopped().is_some() { 1 } else { code }
+    if store.incomplete() { 1 } else { code }
 }
 
 pub(crate) fn read(ctx: &mut Ctx) -> Result<i32, Error> {
```

Apply:

```diff
--- a/src/cli/write.rs
+++ b/src/cli/write.rs
@@ -11,7 +11,7 @@ use crate::frame::Framer;
 use crate::replay::{Cells, Markers};
 use crate::store::Row;
 use crate::write::{Record, Writer, tombstones};
-use crate::{Error, Scope, Warning};
+use crate::{Error, Scope, Store, Warning};
 
 fn writer(ctx: &Ctx, member_size: Option<u64>) -> Result<Writer, Error> {
     let mut w = Writer::open(&ctx.store)?
@@ -130,8 +130,8 @@ fn print(ctx: &mut Ctx, key: &str, cells: &[String]) -> Result<(), Error> {
 pub(crate) fn pop(ctx: &mut Ctx) -> Result<i32, Error> {
     let requested = ctx.args.args[0].clone();
     let store = ctx.open(Scope::Keys(vec![requested.clone()]), false)?;
-    if store.stopped().is_some() {
-        return Ok(1); // no change is based on a read that stopped (§4.10)
+    if store.incomplete() {
+        return Ok(1); // no change is based on a read that failed (§4.10)
     }
     let key = store.lookup_key(&requested).to_string();
     let Some(row) = store.get(&key) else {
@@ -148,7 +148,7 @@ pub(crate) fn pop(ctx: &mut Ctx) -> Result<i32, Error> {
 /// `popitem [first|last]` (§20.2).
 pub(crate) fn popitem(ctx: &mut Ctx) -> Result<i32, Error> {
     let store = ctx.open(Scope::All, false)?;
-    if store.stopped().is_some() {
+    if store.incomplete() {
         return Ok(1);
     }
     let pick = if ctx.args.args.first().map(String::as_str) == Some("first") {
@@ -179,7 +179,7 @@ pub(crate) fn setdefault(ctx: &mut Ctx) -> Result<i32, Error> {
         Err(Error::Missing(_)) => None,
         Err(e) => return Err(e),
     };
-    if store.as_ref().is_some_and(|s| s.stopped().is_some()) {
+    if store.as_ref().is_some_and(Store::incomplete) {
         return Ok(1);
     }
     let markers = store
```

- [ ] **Step 4: Run the tests to see them pass**

Run: `cargo test --all-features 2>&1 | grep -E '^test result|FAILED|panicked'`
Expected: every `test result: ok`; 149 passed in total (the library's unit tests go from 110 to 114: 3 store, 1 maint; the CLI tests from 27 to 28).

- [ ] **Step 5: Write the failing test for a refresh after an early stop**

Apply:

```diff
--- a/src/store.rs
+++ b/src/store.rs
@@ -1082,6 +1087,24 @@ mod tests {
         assert!(s.unreadable().is_empty() && !s.incomplete());
     }
 
+    #[test]
+    fn a_read_that_stopped_before_the_last_part_refreshes_without_panicking() {
+        let dir = tempfile::tempdir().unwrap();
+        let p = write(
+            dir.path(),
+            "s.tsvz",
+            b"#_on_corruption_#\tstop\na\t1\n\0x\t1\n",
+        );
+        let last = write(dir.path(), "s.tsvz.1", b"b\t2\n");
+        let mut s = Store::open(&p, Options::default()).unwrap();
+        assert_eq!(keys(&s), vec!["a"]);
+        assert_eq!(s.refresh().unwrap(), Refreshed::Unchanged);
+        append(&last, b"c\t3\n");
+        assert_eq!(s.refresh().unwrap(), Refreshed::Reloaded);
+        assert_eq!(keys(&s), vec!["a"]);
+        assert!(s.stopped().is_some());
+    }
+
     #[test]
     fn a_skipped_last_part_reloads_on_any_change() {
         let dir = tempfile::tempdir().unwrap();
```

Run: `cargo test --all-features --lib store::tests::a_read_that_stopped 2>&1 | grep -E 'panicked|test result'`
Expected: FAIL, `panicked at src/store.rs:…: index out of bounds: the len is 1 but the index is 1`.

- [ ] **Step 6: Stamp the parts a stop left unread**

Apply:

```diff
--- a/src/store.rs
+++ b/src/store.rs
@@ -344,6 +344,11 @@ impl Store {
                 break; // a stop policy ends the read of the whole store (§4.10)
             }
         }
+        // Parts a stop left unread are stamped too, so that `refresh` sees whether they change.
+        for part in &list[self.follow.stamps.len()..] {
+            let meta = std::fs::metadata(&part.path).ok();
+            self.follow.stamps.push(meta.map(|m| Stamp::of(&m)));
+        }
         self.parts = list
             .into_iter()
             .enumerate()
@@ -609,7 +614,7 @@ impl Store {
     fn follow_append(&mut self, stamp: Stamp) -> Result<bool, Error> {
         let last = self.parts.len() - 1;
         let path = self.parts[last].path.clone();
-        let Some(old) = self.follow.stamps[last] else {
+        let Some(old) = self.follow.stamps.get(last).copied().flatten() else {
             return Ok(false);
         };
         let offset = self.follow.offset;
```

Run: `cargo test --all-features 2>&1 | grep -E '^test result|FAILED|panicked'; cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -1`
Expected: 150 passed, 0 failed; clippy prints no warning.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "Store: skip a part that cannot be read when asked; the CLI prints what it read and exits 1 (§21.8.5, §20.8.1). Refresh no longer panics after a stop before the last part.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_0198xw4TRnucPbiyi9csx4uj"
```

---

### Task 2: Shared operations (`ops`)

Each §20.2 operation's output and exit status is computed once, so the handler answers exactly what the command line prints on the files (§20.8.1). Reading operations emit into an `Out` sink; `pop`, `popitem` and `setdefault` return a `Plan` their caller carries out. The CLI's `Emitter` and `Fields` become one `Printer` that implements `Out`; its behaviour does not change, and the existing CLI tests are the safety net for the refactor.

**Files:**
- Create: `src/ops.rs`
- Modify: `src/lib.rs`, `src/cli/args.rs`; replace `src/cli/output.rs`, `src/cli/read.rs`, `src/cli/write.rs`
- Test: `src/ops.rs` (unit tests); `tests/cli.rs` unchanged and still green

**Interfaces:**
- Consumes: `Store::lookup_key`, `get`, `missing`, `iter`, `first`, `last`, `incomplete`, `width`, `markers`, `mismatches`; `parts::list`; `write::{Record, encode_record, tombstones}`; `escape`; `frame::Framer`.
- Produces (all `pub` in `crate::ops`, `#[doc(hidden)]`):
  - `trait Out { fn row<'a>(&mut self, key: &str, cells: impl Iterator<Item = &'a str>) -> Result<(), Error>; fn fields(&mut self, fields: &[&str]) -> Result<(), Error>; fn count(&mut self, n: usize) -> Result<(), Error>; }`
  - `fn check_args(op: &str, args: &[String]) -> Result<(), String>` (the §20.2 arity and empty-KEY rules; `serve` and `stop` take no arguments)
  - `fn status(store: &Store, code: i32) -> i32`; `fn read/keys/len(store, out) -> Result<i32, Error>`; `fn get(store, keys: &[String], out)`; `fn has(store, key) -> i32`; `fn verify(store, out)`; `fn parts(store: &Path, out) -> Result<i32, Error>` (`Error::Missing` when the store has no part)
  - `enum Plan { Exit(i32), Show { key, cells }, Take { key, cells, records, member_size }, Put { key, cells, record, member_size } }`; `fn pop(store, requested) -> Plan`; `fn popitem(store, first: bool) -> Plan`; `fn setdefault(store: Option<&Store>, format: Format, requested: &str, values: &[String]) -> Plan`
  - `fn parse_records(input: &[u8], format: Format, keys_only: bool, source: &Path, warn: Option<&WarnFn>) -> Vec<Record>`
  - `fn protocol_record(record: &Record) -> String` (a record as a protocol line, no `\n`)
  - `fn record_line(buf, key, cells, delimiter, strict)`, `fn fields_line(buf, fields, first_is_key: bool)` (append a line with `\n`)
  - `cli::output::Printer` with `rows(ctx)`, `fields(ctx, header)`, `count(ctx)`, `finish(self)`; `cli::write::bulk(ctx, keys_only)` is `pub(crate)`.

- [ ] **Step 1: Write the failing tests**

Apply:

```diff
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -25,6 +25,8 @@ pub mod maint;
 #[doc(hidden)]
 pub mod name;
 #[doc(hidden)]
+pub mod ops;
+#[doc(hidden)]
 pub mod parts;
 #[doc(hidden)]
 pub mod replay;
```

Create `src/ops.rs` holding only its test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::{Arc, Mutex};

    use crate::{Options, Scope};

    /// Output as protocol-style lines, for checking what an operation emits.
    #[derive(Default)]
    struct Lines(Vec<String>);

    impl Out for Lines {
        fn row<'a>(
            &mut self,
            key: &str,
            cells: impl Iterator<Item = &'a str>,
        ) -> Result<(), Error> {
            let mut line = String::new();
            record_line(&mut line, key, cells, b'\t', true);
            line.pop();
            self.0.push(line);
            Ok(())
        }
        fn fields(&mut self, fields: &[&str]) -> Result<(), Error> {
            let mut line = String::new();
            fields_line(&mut line, fields, true);
            line.pop();
            self.0.push(line);
            Ok(())
        }
        fn count(&mut self, n: usize) -> Result<(), Error> {
            self.0.push(n.to_string());
            Ok(())
        }
    }

    const TSVZ: Format = Format {
        delimiter: b'\t',
        strict: true,
    };

    fn open(content: &str) -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(&p, content).unwrap();
        let options = Options {
            skip_unreadable: true,
            ..Options::default()
        };
        let s = Store::open(&p, options).unwrap();
        (dir, s)
    }

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn arguments_follow_section_20_2() {
        let ok = |op: &str, args: &[&str]| check_args(op, &strings(args));
        assert!(ok("read", &[]).is_ok() && ok("stop", &[]).is_ok());
        assert!(ok("get", &["a", "b"]).is_ok() && ok("set", &["-"]).is_ok());
        assert!(ok("popitem", &["first"]).is_ok() && ok("popitem", &[]).is_ok());
        for (op, args, want) in [
            ("len", &["x"][..], "takes no arguments"),
            ("serve", &["x"], "takes no arguments"),
            ("popitem", &["middle"], "first or last"),
            ("has", &["a", "b"], "one KEY"),
            ("pop", &["a", "b"], "one KEY"),
            ("setdefault", &["k"], "a KEY and a VALUE"),
            ("get", &[], "needs a KEY"),
            ("delete", &["a", ""], "must not be empty"),
            ("set", &["", "v"], "must not be empty"),
        ] {
            let err = ok(op, args).unwrap_err();
            assert!(err.contains(want), "{op} {args:?}: {err}");
        }
        // Only the KEY of set and setdefault must be non-empty; their values may be empty.
        assert!(ok("set", &["k", ""]).is_ok() && ok("setdefault", &["k", ""]).is_ok());
    }

    #[test]
    fn reads_emit_rows_and_fail_when_incomplete() {
        let (_d, s) = open("#_defaults_#\td1\td2\n<#>h\tv\na\tx\n");
        let mut out = Lines::default();
        assert_eq!(read(&s, &mut out).unwrap(), 0);
        assert_eq!(out.0, ["<#>h\tv\td2", "a\tx\td2"]);
        let mut out = Lines::default();
        assert_eq!(get(&s, &strings(&["a  ", "zz"]), &mut out).unwrap(), 3);
        assert_eq!(out.0, ["a\tx\td2", "zz\td1\td2"]);
        assert_eq!((has(&s, "a"), has(&s, "zz")), (0, 3));
        let mut out = Lines::default();
        assert_eq!(len(&s, &mut out).unwrap(), 0);
        assert_eq!(keys(&s, &mut out).unwrap(), 0);
        assert_eq!(out.0, ["2", "<#>h", "a"]);
        let (_d, stopped) = open("#_on_corruption_#\tstop\na\t1\n\0\0c\t3\n");
        let mut out = Lines::default();
        assert_eq!(read(&stopped, &mut out).unwrap(), 1);
        assert_eq!(out.0, ["a\t1"]);
        assert_eq!(has(&stopped, "a"), 1);
    }

    #[test]
    fn verify_and_parts_emit_fields() {
        let (d, s) = open("#_checksum_crc32_#\na\tx\n#_checksum_crc32_#\t00000000\n");
        drop(s);
        let p = d.path().join("s.tsvz");
        let verifying = Options {
            scope: Scope::KeysOnly,
            verify: true,
            ..Options::default()
        };
        let s = Store::open(&p, verifying).unwrap();
        let mut out = Lines::default();
        assert_eq!(verify(&s, &mut out).unwrap(), 4);
        assert_eq!(out.0.len(), 1);
        assert!(
            out.0[0].ends_with("\t3\tcrc32\t00000000\taa3c6e15"),
            "{:?}",
            out.0
        );
        let mut out = Lines::default();
        assert_eq!(parts(&p, &mut out).unwrap(), 0);
        assert_eq!(out.0, [format!("0\t\t{}\tactive", p.display())]);
        assert!(matches!(
            parts(&d.path().join("none.tsvz"), &mut out),
            Err(Error::Missing(_))
        ));
    }

    #[test]
    fn pop_and_popitem_plan_a_printed_row_then_tombstones() {
        let (_d, s) = open("a\t1\nb \t2\nc\t3\n");
        match pop(&s, "a  ") {
            Plan::Take {
                key,
                cells,
                records,
                ..
            } => {
                assert_eq!((key.as_str(), cells), ("a", strings(&["1"])));
                assert_eq!(records, [Record::Tombstone { key: "a".into() }]);
            }
            _ => panic!("pop a"),
        }
        assert!(matches!(pop(&s, "zz"), Plan::Exit(3)));
        match popitem(&s, true) {
            Plan::Take { key, .. } => assert_eq!(key, "a"),
            _ => panic!("popitem first"),
        }
        match popitem(&s, false) {
            Plan::Take { key, .. } => assert_eq!(key, "c"),
            _ => panic!("popitem last"),
        }
        let (_d, empty) = open("");
        assert!(matches!(popitem(&empty, false), Plan::Exit(3)));
        let (_d, stopped) = open("#_on_corruption_#\tstop\na\t1\n\0\0c\t3\n");
        assert!(matches!(pop(&stopped, "a"), Plan::Exit(1)));
        assert!(matches!(popitem(&stopped, false), Plan::Exit(1)));
    }

    #[test]
    fn setdefault_shows_a_live_row_or_plans_the_padded_new_one() {
        let (_d, s) = open("a\t1\t2\t3\n");
        match setdefault(Some(&s), TSVZ, "a ", &strings(&["9"])) {
            Plan::Show { key, cells } => {
                assert_eq!((key.as_str(), cells), ("a", strings(&["1", "2", "3"])))
            }
            _ => panic!("live"),
        }
        match setdefault(Some(&s), TSVZ, "n ", &strings(&["v "])) {
            Plan::Put {
                key, cells, record, ..
            } => {
                assert_eq!((key.as_str(), cells), ("n", strings(&["v", "", ""])));
                assert_eq!(
                    record,
                    Record::Data {
                        key: "n ".into(),
                        values: strings(&["v "])
                    }
                );
            }
            _ => panic!("missing"),
        }
        match setdefault(None, TSVZ, "k", &strings(&["v"])) {
            Plan::Put { cells, .. } => assert_eq!(cells, strings(&["v"])),
            _ => panic!("no store"),
        }
        let (_d, stopped) = open("#_on_corruption_#\tstop\na\t1\n\0\0c\t3\n");
        assert!(matches!(
            setdefault(Some(&stopped), TSVZ, "n", &strings(&["v"])),
            Plan::Exit(1)
        ));
    }

    #[test]
    fn records_parse_in_the_variant_given() {
        let warned = Arc::new(Mutex::new(Vec::new()));
        let w = warned.clone();
        let warn: WarnFn = Arc::new(move |x: &Warning| w.lock().unwrap().push(x.clone()));
        let csvz = Format {
            delimiter: b',',
            strict: true,
        };
        let input = b"a,x<sep>y\n#comment\n#_defaults_#,d\n<#>h,v\n,skipped\nb\n\ntorn";
        let got = parse_records(input, csvz, false, Path::new("standard input"), Some(&warn));
        assert_eq!(
            got,
            [
                Record::Data {
                    key: "a".into(),
                    values: strings(&["x,y"])
                },
                Record::Marker {
                    key: "#_defaults_#".into(),
                    values: strings(&["d"])
                },
                Record::Data {
                    key: "#h".into(),
                    values: strings(&["v"])
                },
                Record::Tombstone { key: "b".into() },
            ]
        );
        assert_eq!(warned.lock().unwrap().len(), 1, "the torn last line");
        let keys = parse_records(b"a,x\n#_defaults_#,d\n", csvz, true, Path::new("-"), None);
        assert_eq!(
            keys,
            [
                Record::Tombstone { key: "a".into() },
                Record::Marker {
                    key: "#_defaults_#".into(),
                    values: vec![]
                },
            ]
        );
    }

    #[test]
    fn protocol_records_use_tsvz_encoding() {
        let data = Record::Data {
            key: "#k".into(),
            values: strings(&["a\tb", "c\nd", ""]),
        };
        assert_eq!(protocol_record(&data), "<#>k\ta<sep>b\tc<LF>d\t");
        let marker = Record::Marker {
            key: "#_defaults_#".into(),
            values: strings(&["x,y"]),
        };
        assert_eq!(protocol_record(&marker), "#_defaults_#\tx,y");
        assert_eq!(protocol_record(&Record::Tombstone { key: "k".into() }), "k");
    }
}
```

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test --all-features --lib ops:: 2>&1 | grep -E '^error' | sort | uniq -c | head`
Expected: compile errors: `cannot find trait Out`, `cannot find function check_args`, `get`, `has`, `parse_records`, and the rest.

- [ ] **Step 3: Implement `ops`**

Put this above the test module in `src/ops.rs`:

```rust
//! The operations of §20.2, computed once for the command line on the files and for the
//! handler (§21.7), so that what a handler answers is what the files give (§20.8.1).
//!
//! Reading operations emit into an [`Out`] and return the exit status. `pop`, `popitem` and
//! `setdefault` only decide: they return a [`Plan`] their caller carries out, the command line
//! with a `Writer`, the handler through its writer thread.

use std::path::Path;

use crate::escape::{self, is_marker_key};
use crate::frame::Framer;
use crate::name::Format;
use crate::replay::{Cells, Markers};
use crate::store::Row;
use crate::write::{Record, encode_record, tombstones};
use crate::{Error, Store, WarnFn, Warning, parts};

/// Where an operation's output goes: the command line's records or table, or the lines of a
/// handler's response.
pub trait Out {
    /// One row: its key, then its resolved cells (none for `keys`).
    fn row<'a>(&mut self, key: &str, cells: impl Iterator<Item = &'a str>) -> Result<(), Error>;
    /// One line of plain fields (`verify`, `parts`).
    fn fields(&mut self, fields: &[&str]) -> Result<(), Error>;
    /// The number `len` prints.
    fn count(&mut self, n: usize) -> Result<(), Error>;
}

/// The arity and KEY rules of §20.2 for OP and its ARGs (what follows STORE, or the operation
/// of a request, §21.5); the error is the usage message (exit status 2).
pub fn check_args(op: &str, args: &[String]) -> Result<(), String> {
    let n = args.len();
    if matches!(
        op,
        "read" | "clear" | "scrub" | "verify" | "parts" | "len" | "keys" | "serve" | "stop"
    ) && n > 0
    {
        return Err(format!("{op} takes no arguments after STORE"));
    }
    if op == "popitem" && !(n == 0 || (n == 1 && matches!(args[0].as_str(), "first" | "last"))) {
        return Err("popitem takes first or last".into());
    }
    if matches!(op, "has" | "pop") && n > 1 {
        return Err(format!("{op} takes one KEY"));
    }
    if op == "setdefault" && n == 1 {
        return Err("setdefault needs a KEY and a VALUE".into());
    }
    if matches!(
        op,
        "get" | "set" | "append" | "delete" | "has" | "pop" | "setdefault"
    ) {
        if n == 0 {
            return Err(format!("{op} needs a KEY"));
        }
        let keys = if matches!(op, "get" | "delete") {
            args
        } else {
            &args[..1]
        };
        if keys.iter().any(String::is_empty) {
            return Err(format!("{op}: a KEY must not be empty"));
        }
    }
    Ok(())
}

/// A read that skipped a part or stopped at damaged data fails (§4.10, §20.6, §21.8.5).
pub fn status(store: &Store, code: i32) -> i32 {
    if store.incomplete() { 1 } else { code }
}

/// `read`: every live row (§20.2).
pub fn read(store: &Store, out: &mut impl Out) -> Result<i32, Error> {
    for (key, row) in store.iter() {
        out.row(key, row.cells())?;
    }
    Ok(status(store, 0))
}

/// `get KEY ...`: the rows of KEYs, a missing key's defaults row (§14.5); 3 when one is missing.
pub fn get(store: &Store, keys: &[String], out: &mut impl Out) -> Result<i32, Error> {
    let mut missing = false;
    for requested in keys {
        let key = store.lookup_key(requested);
        match store.get(key) {
            Some(row) => out.row(key, row.cells())?,
            None => {
                missing = true;
                if let Some(row) = store.missing() {
                    out.row(key, row.cells())?;
                }
            }
        }
    }
    Ok(status(store, if missing { 3 } else { 0 }))
}

/// `has KEY`: 0 when KEY is live, 3 when not.
pub fn has(store: &Store, key: &str) -> i32 {
    let live = store.get(store.lookup_key(key)).is_some();
    status(store, if live { 0 } else { 3 })
}

/// `len`: the number of live keys.
pub fn len(store: &Store, out: &mut impl Out) -> Result<i32, Error> {
    out.count(store.len())?;
    Ok(status(store, 0))
}

/// `keys`: the live keys, in order.
pub fn keys(store: &Store, out: &mut impl Out) -> Result<i32, Error> {
    for (key, _) in store.iter() {
        out.row(key, std::iter::empty())?;
    }
    Ok(status(store, 0))
}

/// `verify` of a store read with `verify` on: one line per mismatch (§15.3); 4 when any.
pub fn verify(store: &Store, out: &mut impl Out) -> Result<i32, Error> {
    for m in store.mismatches() {
        let part = m.part.display().to_string();
        out.fields(&[&part, &m.line.to_string(), m.algo, &m.expected, &m.computed])?;
    }
    let code = if store.mismatches().is_empty() { 0 } else { 4 };
    Ok(status(store, code))
}

/// `parts`: index, the ordinal as written, path, and the flags `active` and the codec name.
pub fn parts(store: &Path, out: &mut impl Out) -> Result<i32, Error> {
    let list = parts::list(store)?;
    if list.is_empty() {
        return Err(Error::Missing(store.to_path_buf()));
    }
    let last = list.len() - 1;
    for (i, part) in list.iter().enumerate() {
        let mut flags = Vec::new();
        if i == last {
            flags.push("active");
        }
        if let Some(codec) = part.codec {
            flags.push(codec.name());
        }
        let path = part.path.display().to_string();
        out.fields(&[
            &i.to_string(),
            part.ordinal.as_deref().unwrap_or(""),
            &path,
            &flags.join(","),
        ])?;
    }
    Ok(0)
}

/// What `pop`, `popitem` or `setdefault` decided, for its caller to carry out.
#[derive(Debug)]
pub enum Plan {
    /// Exit with this status; nothing to print or write.
    Exit(i32),
    /// Print the row (key first, then `cells`); nothing to write.
    Show { key: String, cells: Vec<String> },
    /// Print the row, then append `records` (`pop`, `popitem`): a row that cannot be printed is
    /// never deleted.
    Take {
        key: String,
        cells: Vec<String>,
        records: Vec<Record>,
        member_size: u64,
    },
    /// Append `record`, then print the row a reader then returns (`setdefault` of a missing key).
    Put {
        key: String,
        cells: Vec<String>,
        record: Record,
        member_size: u64,
    },
}

/// Tombstones that delete exactly `key` from `store` (§9.6).
fn take(store: &Store, key: &str, cells: Vec<String>) -> Plan {
    let strip = store.format().strict && store.markers().strip;
    Plan::Take {
        key: key.to_string(),
        cells,
        records: tombstones([key], strip, store.format().delimiter),
        member_size: store.markers().member_size,
    }
}

/// `pop KEY`: KEY's row and its tombstone; 3 when KEY is missing. No change is based on a read
/// that failed (§4.10).
pub fn pop(store: &Store, requested: &str) -> Plan {
    if store.incomplete() {
        return Plan::Exit(1);
    }
    let key = store.lookup_key(requested);
    match store.get(key) {
        Some(row) => take(store, key, row.to_vec()),
        None => Plan::Exit(3),
    }
}

/// `popitem [first|last]`: the first or last live row and its tombstone; 3 when the store is empty.
pub fn popitem(store: &Store, first: bool) -> Plan {
    if store.incomplete() {
        return Plan::Exit(1);
    }
    let pick = if first { store.first() } else { store.last() };
    match pick {
        Some((key, row)) => take(store, key, row.to_vec()),
        None => Plan::Exit(3),
    }
}

/// `setdefault KEY VALUE ...`: KEY's row when it is live; otherwise the record to append and the
/// row a reader then returns, padded to the store's width, or, in a store with no data row yet,
/// to the width this row sets. `store` is None when the store does not exist yet.
pub fn setdefault(
    store: Option<&Store>,
    format: Format,
    requested: &str,
    values: &[String],
) -> Plan {
    if store.is_some_and(Store::incomplete) {
        return Plan::Exit(1);
    }
    let markers = store.map_or_else(Markers::default, |s| s.markers().clone());
    let strip = format.strict && markers.strip;
    let trim = |s: &str| {
        if strip {
            s.trim_end_matches([' ', '\t']).to_string()
        } else {
            s.to_string()
        }
    };
    let key = trim(requested);
    if let Some(row) = store.and_then(|s| s.get(&key)) {
        let cells = row.to_vec();
        return Plan::Show { key, cells };
    }
    let written = Cells::from_values(values.iter().map(|v| trim(v)));
    let width = store.map_or(0, Store::width);
    let cells = Row::new(&written, &markers.defaults, markers.fill_empty, width).to_vec();
    Plan::Put {
        key,
        cells,
        record: Record::Data {
            key: requested.to_string(),
            values: values.to_vec(),
        },
        member_size: markers.member_size,
    }
}

/// Records read from bulk input in `format` (§20.3.1, §21.7.2): a line is a record; a marker
/// key is a marker line; a comment, an empty line or an empty key is skipped; a key alone is a
/// tombstone. With `keys_only` (`delete -`) only the first field counts. An unterminated last
/// line is ignored with a warning naming `source`.
pub fn parse_records(
    input: &[u8],
    format: Format,
    keys_only: bool,
    source: &Path,
    warn: Option<&WarnFn>,
) -> Vec<Record> {
    let d = format.delimiter;
    let decode = |f: &str| {
        if format.strict {
            escape::decode(f, d).into_owned()
        } else {
            f.to_string()
        }
    };
    let mut records = Vec::new();
    let mut framer = Framer::new();
    framer.push(input, &mut |line| {
        let text = String::from_utf8_lossy(line.text());
        if text.is_empty() {
            return true;
        }
        let mut fields = text.split(d as char);
        let first = fields.next().unwrap_or("");
        let marker = format.strict && is_marker_key(first);
        if first.starts_with('#') && !marker {
            return true; // a comment
        }
        if marker {
            let values = if keys_only {
                Vec::new()
            } else {
                fields.map(&decode).collect()
            };
            records.push(Record::Marker {
                key: first.to_string(),
                values,
            });
            return true;
        }
        let key = decode(first);
        if key.is_empty() {
            return true; // a reader ignores an empty key (§8.4)
        }
        let values: Vec<String> = if keys_only {
            Vec::new()
        } else {
            fields.map(&decode).collect()
        };
        records.push(if values.is_empty() {
            Record::Tombstone { key }
        } else {
            Record::Data { key, values }
        });
        true
    });
    if !framer.tail().is_empty() {
        if let Some(w) = warn {
            w(&Warning {
                part: source.to_path_buf(),
                offset: None,
                message: "ignored the unterminated last line".into(),
            });
        }
    }
    records
}

const PROTOCOL: Format = Format {
    delimiter: b'\t',
    strict: true,
};

/// A record as a line of the handler protocol, without its `\n` (§21.4): a marker key as
/// written, a data key with `<#>` for a leading `#`, values §13-encoded with TAB.
pub fn protocol_record(record: &Record) -> String {
    let mut line = String::new();
    // A strict variant encodes every field, so this cannot be refused.
    let _ = encode_record(record, PROTOCOL, &mut line);
    line.pop();
    line
}

/// Append one row as a record line to `buf`: the key (`<#>` for a leading `#`), then its cells,
/// joined by `delimiter` and ended by `\n`. Strict variants are §13-encoded; a loose file's
/// cells are written as they are (§20.4.4).
pub fn record_line<'a>(
    buf: &mut String,
    key: &str,
    cells: impl Iterator<Item = &'a str>,
    delimiter: u8,
    strict: bool,
) {
    if strict {
        escape::encode_into(buf, key, delimiter, true);
    } else {
        buf.push_str(key);
    }
    for cell in cells {
        buf.push(delimiter as char);
        if strict {
            escape::encode_into(buf, cell, delimiter, false);
        } else {
            buf.push_str(cell);
        }
    }
    buf.push('\n');
}

/// Append a `verify` or `parts` line to `buf`: fields joined by TAB, each §13-encoded, ended by
/// `\n`. With `first_is_key` a leading `#` of the first field is written `<#>`, as a handler's
/// output lines need (§21.6.2).
pub fn fields_line(buf: &mut String, fields: &[&str], first_is_key: bool) {
    for (i, f) in fields.iter().enumerate() {
        if i > 0 {
            buf.push('\t');
        }
        escape::encode_into(buf, f, b'\t', first_is_key && i == 0);
    }
    buf.push('\n');
}
```

- [ ] **Step 4: Run the `ops` tests**

Run: `cargo test --all-features --lib ops:: 2>&1 | grep -E 'test result|panicked'`
Expected: `test result: ok. 7 passed`.

- [ ] **Step 5: Move the CLI onto `ops`**

`args.rs` keeps parsing and calls `ops::check_args`.

Apply:

```diff
--- a/src/cli/args.rs
+++ b/src/cli/args.rs
@@ -194,48 +194,10 @@ pub fn parse(argv: &[String]) -> Result<Parsed, String> {
             }
         }
     }
-    validate(&a)?;
+    crate::ops::check_args(&a.op, &a.args)?;
     Ok(Parsed::Run(a))
 }
 
-fn validate(a: &Args) -> Result<(), String> {
-    let op = a.op.as_str();
-    let n = a.args.len();
-    if matches!(
-        op,
-        "read" | "clear" | "scrub" | "verify" | "parts" | "len" | "keys" | "serve" | "stop"
-    ) && n > 0
-    {
-        return Err(format!("{op} takes no arguments after STORE"));
-    }
-    if op == "popitem" && !(n == 0 || (n == 1 && matches!(a.args[0].as_str(), "first" | "last"))) {
-        return Err("popitem takes first or last".into());
-    }
-    if matches!(op, "has" | "pop") && n > 1 {
-        return Err(format!("{op} takes one KEY"));
-    }
-    if op == "setdefault" && n == 1 {
-        return Err("setdefault needs a KEY and a VALUE".into());
-    }
-    if matches!(
-        op,
-        "get" | "set" | "append" | "delete" | "has" | "pop" | "setdefault"
-    ) {
-        if n == 0 {
-            return Err(format!("{op} needs a KEY"));
-        }
-        let keys = if matches!(op, "get" | "delete") {
-            &a.args[..]
-        } else {
-            &a.args[..1]
-        };
-        if keys.iter().any(String::is_empty) {
-            return Err(format!("{op}: a KEY must not be empty"));
-        }
-    }
-    Ok(())
-}
-
 #[cfg(test)]
 mod tests {
     use super::*;
```

Replace `src/cli/output.rs` with:

```rust
//! Records and table output (§20.4).

use std::io::{self, Write};
use std::path::Path;

use crate::Error;
use crate::cli::{Ctx, table};
use crate::ops::{self, Out};

pub(crate) fn stdout_error(e: io::Error) -> Error {
    Error::io(Path::new("stdout"), e)
}

enum Layout {
    /// Rows: records as they come, or a table at the end.
    Rows,
    /// `verify` and `parts` lines: TAB-joined, or a table under this header (nothing at all
    /// when there are no lines).
    Fields(&'static [&'static str]),
    /// `len`: the count alone.
    Count,
}

/// Prints what an operation emits (`ops::Out`) in the chosen format.
pub(crate) struct Printer<'c> {
    ctx: &'c mut Ctx,
    layout: Layout,
    buf: String,
    table: Option<Vec<Vec<String>>>,
}

impl<'c> Printer<'c> {
    fn new(ctx: &'c mut Ctx, layout: Layout) -> Printer<'c> {
        let table = ctx.table.then(Vec::new);
        Printer {
            ctx,
            layout,
            buf: String::new(),
            table,
        }
    }

    pub(crate) fn rows(ctx: &'c mut Ctx) -> Printer<'c> {
        Printer::new(ctx, Layout::Rows)
    }

    pub(crate) fn fields(ctx: &'c mut Ctx, header: &'static [&'static str]) -> Printer<'c> {
        Printer::new(ctx, Layout::Fields(header))
    }

    pub(crate) fn count(ctx: &'c mut Ctx) -> Printer<'c> {
        Printer::new(ctx, Layout::Count)
    }

    fn write(&mut self) -> Result<(), Error> {
        let result = self.ctx.out.write_all(self.buf.as_bytes());
        self.buf.clear();
        result.map_err(stdout_error)
    }

    /// A table is followed by an empty line, as TSVZ.py prints it.
    pub(crate) fn finish(self) -> Result<(), Error> {
        let Some(rows) = self.table else {
            return Ok(());
        };
        let text = match self.layout {
            Layout::Rows => table::rows_table(&rows) + "\n",
            Layout::Fields(header) if !rows.is_empty() => {
                table::fields_table(header, &rows, table::terminal_columns()) + "\n"
            }
            _ => return Ok(()),
        };
        self.ctx
            .out
            .write_all(text.as_bytes())
            .map_err(stdout_error)
    }
}

impl Out for Printer<'_> {
    fn row<'a>(&mut self, key: &str, cells: impl Iterator<Item = &'a str>) -> Result<(), Error> {
        if let Some(rows) = &mut self.table {
            let mut row = vec![key.to_string()];
            row.extend(cells.map(str::to_string));
            rows.push(row);
            return Ok(());
        }
        let (d, strict) = (self.ctx.format.delimiter, self.ctx.format.strict);
        ops::record_line(&mut self.buf, key, cells, d, strict);
        self.write()
    }

    fn fields(&mut self, fields: &[&str]) -> Result<(), Error> {
        if let Some(rows) = &mut self.table {
            rows.push(fields.iter().map(|f| f.to_string()).collect());
            return Ok(());
        }
        ops::fields_line(&mut self.buf, fields, false);
        self.write()
    }

    fn count(&mut self, n: usize) -> Result<(), Error> {
        writeln!(self.ctx.out, "{n}").map_err(stdout_error)
    }
}
```

Replace `src/cli/read.rs` with:

```rust
//! The reading operations: read, get, has, len, keys, verify, parts (§20.2), through `ops`.

use crate::cli::Ctx;
use crate::cli::output::Printer;
use crate::{Error, Scope, ops};

pub(crate) fn read(ctx: &mut Ctx) -> Result<i32, Error> {
    let store = ctx.open(Scope::All, false)?;
    let mut out = Printer::rows(ctx);
    let code = ops::read(&store, &mut out)?;
    out.finish()?;
    ctx.info(&format!(
        "{} live keys in {} parts",
        store.len(),
        store.parts().len()
    ));
    Ok(code)
}

pub(crate) fn get(ctx: &mut Ctx) -> Result<i32, Error> {
    let keys = ctx.args.args.clone();
    let store = ctx.open(Scope::Keys(keys.clone()), false)?;
    let mut out = Printer::rows(ctx);
    let code = ops::get(&store, &keys, &mut out)?;
    out.finish()?;
    Ok(code)
}

pub(crate) fn has(ctx: &mut Ctx) -> Result<i32, Error> {
    let store = ctx.open(Scope::KeysOnly, false)?;
    Ok(ops::has(&store, &ctx.args.args[0]))
}

pub(crate) fn len(ctx: &mut Ctx) -> Result<i32, Error> {
    let store = ctx.open(Scope::KeysOnly, false)?;
    ops::len(&store, &mut Printer::count(ctx))
}

pub(crate) fn keys(ctx: &mut Ctx) -> Result<i32, Error> {
    let store = ctx.open(Scope::KeysOnly, false)?;
    let mut out = Printer::rows(ctx);
    let code = ops::keys(&store, &mut out)?;
    out.finish()?;
    Ok(code)
}

pub(crate) fn verify(ctx: &mut Ctx) -> Result<i32, Error> {
    let store = ctx.open(Scope::KeysOnly, true)?;
    let mut out = Printer::fields(ctx, &["path", "line", "algorithm", "expected", "computed"]);
    let code = ops::verify(&store, &mut out)?;
    out.finish()?;
    Ok(code)
}

pub(crate) fn parts(ctx: &mut Ctx) -> Result<i32, Error> {
    let store = ctx.store.clone();
    let mut out = Printer::fields(ctx, &["index", "ordinal", "path", "flags"]);
    let code = ops::parts(&store, &mut out)?;
    out.finish()?;
    Ok(code)
}
```

Replace `src/cli/write.rs` with:

```rust
//! The writing operations: set, append, delete (with bulk input), pop, popitem, setdefault
//! (§20.2, §20.3), through `ops`.

use std::io::{Read, Write};
use std::path::Path;

use crate::cli::Ctx;
use crate::cli::output::{Printer, stdout_error};
use crate::escape::is_marker_key;
use crate::ops::{self, Out, Plan};
use crate::write::{Record, Writer};
use crate::{Error, Scope};

fn writer(ctx: &Ctx, member_size: Option<u64>) -> Result<Writer, Error> {
    let mut w = Writer::open(&ctx.store)?
        .warnings(Some(ctx.warn.clone()))
        .delimiter(ctx.args.delimiter);
    if let Some(n) = member_size {
        w = w.member_size(n as usize);
    }
    Ok(w)
}

/// `set` and `append` (§20.2): one record, or the records on standard input for `-`.
pub(crate) fn set(ctx: &mut Ctx) -> Result<i32, Error> {
    let records = if ctx.args.args == ["-"] {
        bulk(ctx, false)?
    } else {
        vec![Record::from_args(&ctx.args.args[0], &ctx.args.args[1..])]
    };
    writer(ctx, None)?.append(&records)?;
    ctx.info(&format!("appended {} records", records.len()));
    Ok(0)
}

/// `delete` (§20.2): one tombstone per key (a marker key is reset), or the keys on standard input.
pub(crate) fn delete(ctx: &mut Ctx) -> Result<i32, Error> {
    let records = if ctx.args.args == ["-"] {
        bulk(ctx, true)?
    } else {
        ctx.args
            .args
            .iter()
            .map(|k| Record::from_args(k, &[]))
            .collect()
    };
    writer(ctx, None)?.append(&records)?;
    ctx.info(&format!("appended {} tombstones", records.len()));
    Ok(0)
}

/// Standard input read as records in the store's variant (§20.3.1).
pub(crate) fn bulk(ctx: &Ctx, keys_only: bool) -> Result<Vec<Record>, Error> {
    let mut input = Vec::new();
    std::io::stdin()
        .lock()
        .read_to_end(&mut input)
        .map_err(|e| Error::io(Path::new("standard input"), e))?;
    let source = Path::new("standard input");
    Ok(ops::parse_records(
        &input,
        ctx.format,
        keys_only,
        source,
        Some(&ctx.warn),
    ))
}

/// Print one row and flush it, so that a row which cannot be printed is never deleted.
fn print(ctx: &mut Ctx, key: &str, cells: &[String]) -> Result<(), Error> {
    let mut out = Printer::rows(ctx);
    out.row(key, cells.iter().map(String::as_str))?;
    out.finish()?;
    ctx.out.flush().map_err(stdout_error)
}

/// Carry out what `pop`, `popitem` or `setdefault` decided.
fn carry_out(ctx: &mut Ctx, plan: Plan) -> Result<i32, Error> {
    match plan {
        Plan::Exit(code) => Ok(code),
        Plan::Show { key, cells } => {
            print(ctx, &key, &cells)?;
            Ok(0)
        }
        Plan::Take {
            key,
            cells,
            records,
            member_size,
        } => {
            print(ctx, &key, &cells)?;
            writer(ctx, Some(member_size))?.append(&records)?;
            Ok(0)
        }
        Plan::Put {
            key,
            cells,
            record,
            member_size,
        } => {
            writer(ctx, Some(member_size))?.append(std::slice::from_ref(&record))?;
            print(ctx, &key, &cells)?;
            Ok(0)
        }
    }
}

/// `pop` (§20.2): print KEY's row and append its tombstone; 3 when it is missing.
pub(crate) fn pop(ctx: &mut Ctx) -> Result<i32, Error> {
    let requested = ctx.args.args[0].clone();
    let store = ctx.open(Scope::Keys(vec![requested.clone()]), false)?;
    carry_out(ctx, ops::pop(&store, &requested))
}

/// `popitem [first|last]` (§20.2).
pub(crate) fn popitem(ctx: &mut Ctx) -> Result<i32, Error> {
    let store = ctx.open(Scope::All, false)?;
    let first = ctx.args.args.first().map(String::as_str) == Some("first");
    carry_out(ctx, ops::popitem(&store, first))
}

/// `setdefault KEY VALUE ...` (§20.2): KEY's row when it is live; otherwise append the record
/// and print the row a reader then returns.
pub(crate) fn setdefault(ctx: &mut Ctx) -> Result<i32, Error> {
    let requested = ctx.args.args[0].clone();
    let values = ctx.args.args[1..].to_vec();
    if is_marker_key(&requested) {
        diag!("tsvz: setdefault: {requested} is a marker key (§21.7.3)");
        return Ok(2);
    }
    let store = match ctx.open(Scope::Keys(vec![requested.clone()]), false) {
        Ok(store) => Some(store),
        Err(Error::Missing(_)) => None,
        Err(e) => return Err(e),
    };
    let plan = ops::setdefault(store.as_ref(), ctx.format, &requested, &values);
    carry_out(ctx, plan)
}
```

- [ ] **Step 6: Run everything**

Run: `cargo test --all-features 2>&1 | grep -E '^test result|FAILED|panicked'; cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -1`
Expected: 157 passed, 0 failed (the 28 CLI tests unchanged); no clippy warning.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "ops: compute each §20.2 operation once, for the command line on the files and for the handler (§20.8.1).

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_0198xw4TRnucPbiyi9csx4uj"
```

---

### Task 3: Naming a key (§20.2.7)

TSVZ 4.7 added §20.2.7. While stripping is in force at the end of a strict store, a KEY whose encoded form (§13) ends in a space or tab names the live key equal to it if there is one (MUST), otherwise the key a reader resolves it to (SHOULD); any other KEY names itself. This applies to the KEYs of `get`, `has`, `pop`, `delete` and `setdefault`, and to the keys `delete -` reads. A `.tsvz` key ending in a tab is written `k<sep>`, which stripping never touches, so `get 'k<TAB>'` now names `k<TAB>` (4.6 stripped the tab). `delete` names all its keys against the store as it was before the batch, guards a tombstone that needs it (§9.6), and a `#_strip_trailing_whites_#` reset inside a bulk delete turns stripping back on. When `delete` must read the store to name a key and the read is incomplete (§4.10), it writes nothing and exits 1. `set` does not change. TSVZ.py's helpers are `_specStrippable`, `_specNameKey`, `_specNamesKeys` and `_specDeleteRows`; this task mirrors them. The handler's `delete` names keys under its lock; that part comes with the engine (Task 5).

**Files:**
- Modify: `src/write.rs`, `src/store.rs`, `src/ops.rs`, `src/cli/write.rs`
- Test: `src/write.rs`, `src/store.rs`, `src/ops.rs` (unit tests), `tests/cli.rs`

**Interfaces:**
- Consumes: `escape::{encode, decode}`, `Store` internals (`table.index`, `markers.strip`), `ops` (Task 2).
- Produces: `write::strippable(key: &str, delimiter: u8) -> bool`; `write::resolve(key: &str, delimiter: u8) -> Cow<'_, str>`; `write::name_key<'k>(key: &'k str, delimiter: u8, strip: bool, live: impl Fn(&str) -> bool) -> Cow<'k, str>`; `Store::lookup_key(&self, requested: &'k str) -> Cow<'k, str>` (was `&'k str`); `Scope::Keys` keeps each requested key and the key a reader resolves it to; `ops::delete_needs_store(records: &[Record], delimiter: u8) -> bool`; `ops::delete_records(records: &[Record], delimiter: u8, strip: bool, live: impl Fn(&str) -> bool) -> Vec<Record>`.

- [ ] **Step 1: Write the failing tests**

Apply:

```diff
--- a/src/write.rs
+++ b/src/write.rs
@@ -503,4 +534,30 @@ mod tests {
         );
         assert_eq!(fs::read(&p).unwrap(), b"k;a\tb\n");
     }
+
+    #[test]
+    fn a_key_names_itself_unless_a_reader_would_strip_it() {
+        let live = |k: &str| k == "k ";
+        assert!(strippable("k ", b'\t') && strippable("k\t", b','));
+        assert!(!strippable("k\t", b'\t'), "written k<sep>");
+        assert!(!strippable("k", b'\t'));
+        assert_eq!(resolve("#k \t ", b','), "#k");
+        assert_eq!(
+            resolve("k\t ", b'\t'),
+            "k\t",
+            "only the encoded form's white space"
+        );
+        assert_eq!(
+            name_key("k ", b'\t', true, live),
+            "k ",
+            "the live key equal to it"
+        );
+        assert_eq!(name_key("x  ", b'\t', true, live), "x");
+        assert_eq!(
+            name_key("x  ", b'\t', false, live),
+            "x  ",
+            "stripping is off"
+        );
+        assert_eq!(name_key("k\t", b'\t', true, live), "k\t");
+    }
 }
```

Apply (two older store tests used a `.tsvz` key ending in a tab, which no longer strips; they now use spaces):

```diff
--- a/src/store.rs
+++ b/src/store.rs
@@ -774,7 +784,12 @@ mod tests {
                 .collect::<Vec<_>>(),
             vec![(0, false), (1, false), (2, true)]
         );
-        assert_eq!(s.lookup_key("k \t"), "k");
+        assert_eq!(s.lookup_key("k  "), "k");
+        assert_eq!(
+            s.lookup_key("k \t"),
+            "k \t",
+            "a tab written <sep> is not stripped (§20.2.7)"
+        );
     }
 
     #[test]
@@ -829,14 +844,14 @@ mod tests {
         let some = Store::open(
             &p,
             Options {
-                scope: Scope::Keys(vec!["b \t".into(), "zz".into()]),
+                scope: Scope::Keys(vec!["b  ".into(), "zz".into()]),
                 ..Options::default()
             },
         )
         .unwrap();
         assert_eq!(some.len(), 1);
         assert_eq!(
-            some.get(some.lookup_key("b \t")).unwrap().to_vec(),
+            some.get(&some.lookup_key("b  ")).unwrap().to_vec(),
             v(&["z", ""])
         );
     }
@@ -1105,6 +1120,49 @@ mod tests {
         assert!(s.stopped().is_some());
     }
 
+    #[test]
+    fn a_requested_key_names_a_key_of_the_store() {
+        // §20.2.7: k<space> was written while stripping was off; stripping is on at the end.
+        let dir = tempfile::tempdir().unwrap();
+        let both = b"#_strip_trailing_whites_#\tfalse\nk \tv1\n#_strip_trailing_whites_#\ttrue\nk\tv2\nt<sep>\tw\n";
+        let p = write(dir.path(), "s.tsvz", both);
+        for scope in [
+            Scope::All,
+            Scope::KeysOnly,
+            Scope::Keys(vec!["k ".into(), "t\t ".into()]),
+        ] {
+            let s = Store::open(
+                &p,
+                Options {
+                    scope: scope.clone(),
+                    ..Options::default()
+                },
+            )
+            .unwrap();
+            assert_eq!(s.lookup_key("k "), "k ", "{scope:?}");
+            assert_eq!(s.lookup_key("k"), "k");
+            assert_eq!(s.lookup_key("t\t "), "t\t", "{scope:?}");
+            assert!(s.get(&s.lookup_key("t\t ")).is_some(), "{scope:?}");
+        }
+        let s = Store::open(&p, Options::default()).unwrap();
+        assert_eq!(
+            s.lookup_key("k  "),
+            "k",
+            "no key k<space><space>: what a reader resolves"
+        );
+        let off = write(
+            dir.path(),
+            "o.tsvz",
+            b"#_strip_trailing_whites_#\tfalse\nk\tv\n",
+        );
+        let s = Store::open(&off, Options::default()).unwrap();
+        assert_eq!(
+            s.lookup_key("k "),
+            "k ",
+            "stripping is off: a key names itself"
+        );
+    }
+
     #[test]
     fn a_skipped_last_part_reloads_on_any_change() {
         let dir = tempfile::tempdir().unwrap();
```

Apply:

```diff
--- a/src/ops.rs
+++ b/src/ops.rs
@@ -575,6 +628,50 @@ mod tests {
         ));
     }
 
+    fn tomb(k: &str) -> Record {
+        Record::Tombstone { key: k.into() }
+    }
+
+    fn strip(value: Option<&str>) -> Record {
+        Record::Marker {
+            key: "#_strip_trailing_whites_#".into(),
+            values: value.map(|v| vec![v.to_string()]).unwrap_or_default(),
+        }
+    }
+
+    #[test]
+    fn delete_names_its_keys_against_the_store_before_it() {
+        let live = |k: &str| k == "k " || k == "k";
+        let d = b'\t';
+        assert!(!delete_needs_store(&[tomb("k"), strip(None)], d));
+        assert!(delete_needs_store(&[tomb("k ")], d));
+        // Both k<space> and k live, stripping on: each KEY names its own key, guarded (§9.6).
+        assert_eq!(
+            delete_records(&[tomb("k "), tomb("k "), tomb("k")], d, true, live),
+            [
+                strip(Some("false")),
+                tomb("k "),
+                tomb("k "),
+                tomb("k"),
+                strip(Some("true"))
+            ]
+        );
+        // A key that is not live names what a reader resolves it to: no guard is needed.
+        assert_eq!(delete_records(&[tomb("x ")], d, true, live), [tomb("x")]);
+        // Stripping off before the batch: KEYs name themselves; a reset inside the batch turns
+        // stripping back on, so a later tombstone that needs it is guarded.
+        assert_eq!(
+            delete_records(&[tomb("k "), strip(None), tomb("k ")], d, false, live),
+            [
+                tomb("k "),
+                strip(None),
+                strip(Some("false")),
+                tomb("k "),
+                strip(Some("true"))
+            ]
+        );
+    }
+
     #[test]
     fn records_parse_in_the_variant_given() {
         let warned = Arc::new(Mutex::new(Vec::new()));
```

Apply:

```diff
--- a/tests/cli.rs
+++ b/tests/cli.rs
@@ -706,3 +706,50 @@ fn an_unreadable_part_exits_1_naming_it() {
         "{err}"
     );
 }
+
+#[test]
+fn a_key_ending_in_white_space_names_the_live_key_equal_to_it() {
+    // §20.2.7: k<space> was written while stripping was off; stripping is on at the end.
+    let both = "#_strip_trailing_whites_#\tfalse\nk \tv1\n#_strip_trailing_whites_#\ttrue\nk\tv2\n";
+    let d = store(both);
+    assert_eq!(records(d.path(), &["get", "s.tsvz", "k "]).1, "k \tv1\n");
+    assert_eq!(records(d.path(), &["get", "s.tsvz", "k"]).1, "k\tv2\n");
+    assert_eq!(
+        records(d.path(), &["setdefault", "s.tsvz", "k ", "z"]).1,
+        "k \tv1\n"
+    );
+    assert_eq!(records(d.path(), &["pop", "s.tsvz", "k "]).1, "k \tv1\n");
+    assert_eq!(records(d.path(), &["read", "s.tsvz"]).1, "k\tv2\n");
+    let d = store(both);
+    assert_eq!(records(d.path(), &["delete", "s.tsvz", "k ", "k "]).0, 0);
+    assert_eq!(records(d.path(), &["read", "s.tsvz"]).1, "k\tv2\n");
+    let d = store(both);
+    let keys = records(d.path(), &["keys", "s.tsvz"]).1;
+    assert_eq!(keys, "k \nk\n");
+    assert_eq!(
+        run_in(d.path(), &["delete", "s.tsvz", "-"], keys.as_bytes()).0,
+        0
+    );
+    assert_eq!(records(d.path(), &["len", "s.tsvz"]).1, "0\n");
+    let d = store("k\tplain\nk<sep>\ttab\n");
+    assert_eq!(
+        records(d.path(), &["get", "s.tsvz", "k\t"]).1,
+        "k<sep>\ttab\n"
+    );
+    assert_eq!(records(d.path(), &["delete", "s.tsvz", "k\t"]).0, 0);
+    assert_eq!(records(d.path(), &["read", "s.tsvz"]).1, "k\tplain\n");
+}
+
+#[test]
+fn a_delete_that_must_read_the_store_writes_nothing_when_it_cannot() {
+    let content = "#_on_corruption_#\tstop\nk\t1\n\0\0c\t3\n";
+    let d = store(content);
+    let (code, out, err) = records(d.path(), &["delete", "s.tsvz", "k "]);
+    assert_eq!((code, out.as_str()), (1, ""), "{err}");
+    assert_eq!(file(d.path(), "s.tsvz"), content);
+    assert_eq!(
+        records(d.path(), &["delete", "s.tsvz", "k"]).0,
+        0,
+        "k names itself: no read"
+    );
+}
```

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test --all-features --lib 2>&1 | grep -E '^error' | sort | uniq -c | head`
Expected: compile errors: `cannot find function strippable`, `resolve`, `name_key`, `delete_needs_store`, `delete_records`, and mismatched types where `lookup_key` is borrowed.

- [ ] **Step 3: Implement naming**

Apply:

```diff
--- a/src/write.rs
+++ b/src/write.rs
@@ -1,5 +1,6 @@
 //! Appending records (§13.3, §16.4, §16.7, §17.7, §18.2).
 
+use std::borrow::Cow;
 use std::fs::{File, OpenOptions};
 use std::io::{self, Write};
 use std::os::unix::fs::FileExt;
@@ -71,11 +72,7 @@ pub fn tombstones<'k>(
         .into_iter()
         .map(|k| Record::Tombstone { key: k.to_string() })
         .collect();
-    let exposed = |r: &Record| {
-        matches!(r, Record::Tombstone { key }
-            if key.ends_with([' ', '\t'])
-                && escape::encode(key, delimiter, true).ends_with([' ', '\t']))
-    };
+    let exposed = |r: &Record| matches!(r, Record::Tombstone { key } if strippable(key, delimiter));
     if strip && records.iter().any(exposed) {
         let set = |value: &str| Record::Marker {
             key: "#_strip_trailing_whites_#".into(),
@@ -87,6 +84,40 @@ pub fn tombstones<'k>(
     records
 }
 
+/// Whether a reader that strips (§7.7) would change `key` in a store with this `delimiter`:
+/// its encoded form (§13) ends in a space or tab.
+pub fn strippable(key: &str, delimiter: u8) -> bool {
+    key.ends_with([' ', '\t']) && escape::encode(key, delimiter, true).ends_with([' ', '\t'])
+}
+
+/// What a reader that strips resolves `key` to (§7.7): the key without the trailing spaces and
+/// tabs of its encoded form.
+pub fn resolve(key: &str, delimiter: u8) -> Cow<'_, str> {
+    if !strippable(key, delimiter) {
+        return Cow::Borrowed(key);
+    }
+    let encoded = escape::encode(key, delimiter, true);
+    let trimmed = encoded.trim_end_matches([' ', '\t']);
+    Cow::Owned(escape::decode(trimmed, delimiter).into_owned())
+}
+
+/// The key a requested `key` names (§20.2.7) in a strict store whose final stripping is
+/// `strip` and whose live keys `live` tells: while stripping is on, a key a reader would strip
+/// names the live key equal to it if there is one, else what a reader resolves it to; while it
+/// is off, a key names itself.
+pub fn name_key<'k>(
+    key: &'k str,
+    delimiter: u8,
+    strip: bool,
+    live: impl Fn(&str) -> bool,
+) -> Cow<'k, str> {
+    if !strip || live(key) {
+        Cow::Borrowed(key)
+    } else {
+        resolve(key, delimiter)
+    }
+}
+
 /// Append `record` and its `\n` to `out`, encoded for `format` (§13.3). A loose file has no
 /// escaping, so a field holding its delimiter, `\n` or `\r` is refused.
 pub fn encode_record(record: &Record, format: Format, out: &mut String) -> Result<(), Error> {
```

Apply:

```diff
--- a/src/store.rs
+++ b/src/store.rs
@@ -1,5 +1,6 @@
 //! A store: the ordered map of live rows after replaying its parts (§3, §14, §17).
 
+use std::borrow::Cow;
 use std::collections::HashSet;
 use std::fs::File;
 use std::io::{Read, Seek, SeekFrom};
@@ -145,11 +146,20 @@ struct Table {
 }
 
 impl Table {
-    fn new(scope: &Scope) -> Table {
+    /// A table for `scope`; under `Scope::Keys` it keeps each requested key and, for a strict
+    /// store, the key a reader resolves it to (§20.2.7).
+    fn new(scope: &Scope, format: Format) -> Table {
         let wanted = match scope {
             Scope::Keys(keys) => Some(
                 keys.iter()
-                    .flat_map(|k| [k.clone(), k.trim_end_matches([' ', '\t']).to_string()])
+                    .flat_map(|k| {
+                        let resolved = if format.strict {
+                            crate::write::resolve(k, format.delimiter).into_owned()
+                        } else {
+                            k.clone()
+                        };
+                        [k.clone(), resolved]
+                    })
                     .collect(),
             ),
             _ => None,
@@ -300,7 +310,7 @@ impl Store {
         }
         let mut store = Store {
             replay: Replay::new(format, options.verify, options.warnings.clone()),
-            table: Table::new(&options.scope),
+            table: Table::new(&options.scope, format),
             path,
             format,
             options,
@@ -476,14 +486,14 @@ impl Store {
         &self.replay.markers
     }
 
-    /// A requested key as §7.7 resolves a key: trailing space and tab removed while the final
-    /// stripping is on (§20.2 `get`).
-    pub fn lookup_key<'k>(&self, requested: &'k str) -> &'k str {
-        if self.format.strict && self.replay.markers.strip {
-            requested.trim_end_matches([' ', '\t'])
-        } else {
-            requested
-        }
+    /// The key a requested KEY names (§20.2.7): while the final stripping is on, a key a reader
+    /// would strip names the live key equal to it, else what a reader resolves it to (§7.7).
+    /// A loose file's keys name themselves.
+    pub fn lookup_key<'k>(&self, requested: &'k str) -> Cow<'k, str> {
+        let strip = self.format.strict && self.replay.markers.strip;
+        crate::write::name_key(requested, self.format.delimiter, strip, |k| {
+            self.table.index.contains_key(k)
+        })
     }
 
     pub fn get(&self, key: &str) -> Option<Row<'_>> {
```

Apply:

```diff
--- a/src/ops.rs
+++ b/src/ops.rs
@@ -12,7 +12,7 @@ use crate::frame::Framer;
 use crate::name::Format;
 use crate::replay::{Cells, Markers};
 use crate::store::Row;
-use crate::write::{Record, encode_record, tombstones};
+use crate::write::{Record, encode_record, name_key, strippable, tombstones};
 use crate::{Error, Store, WarnFn, Warning, parts};
 
 /// Where an operation's output goes: the command line's records or table, or the lines of a
@@ -83,12 +83,12 @@ pub fn get(store: &Store, keys: &[String], out: &mut impl Out) -> Result<i32, Er
     let mut missing = false;
     for requested in keys {
         let key = store.lookup_key(requested);
-        match store.get(key) {
-            Some(row) => out.row(key, row.cells())?,
+        match store.get(&key) {
+            Some(row) => out.row(&key, row.cells())?,
             None => {
                 missing = true;
                 if let Some(row) = store.missing() {
-                    out.row(key, row.cells())?;
+                    out.row(&key, row.cells())?;
                 }
             }
         }
@@ -98,7 +98,7 @@ pub fn get(store: &Store, keys: &[String], out: &mut impl Out) -> Result<i32, Er
 
 /// `has KEY`: 0 when KEY is live, 3 when not.
 pub fn has(store: &Store, key: &str) -> i32 {
-    let live = store.get(store.lookup_key(key)).is_some();
+    let live = store.get(&store.lookup_key(key)).is_some();
     status(store, if live { 0 } else { 3 })
 }
 
@@ -194,8 +194,8 @@ pub fn pop(store: &Store, requested: &str) -> Plan {
         return Plan::Exit(1);
     }
     let key = store.lookup_key(requested);
-    match store.get(key) {
-        Some(row) => take(store, key, row.to_vec()),
+    match store.get(&key) {
+        Some(row) => take(store, &key, row.to_vec()),
         None => Plan::Exit(3),
     }
 }
@@ -233,7 +233,10 @@ pub fn setdefault(
             s.to_string()
         }
     };
-    let key = trim(requested);
+    let key = match store {
+        Some(s) => s.lookup_key(requested).into_owned(),
+        None => name_key(requested, format.delimiter, strip, |_| false).into_owned(),
+    };
     if let Some(row) = store.and_then(|s| s.get(&key)) {
         let cells = row.to_vec();
         return Plan::Show { key, cells };
@@ -252,6 +255,56 @@ pub fn setdefault(
     }
 }
 
+/// Whether naming the keys of a `delete` needs the store (§20.2.7): a data key that a reader
+/// would strip names a key that depends on what is live.
+pub fn delete_needs_store(records: &[Record], delimiter: u8) -> bool {
+    records
+        .iter()
+        .any(|r| matches!(r, Record::Tombstone { key } if strippable(key, delimiter)))
+}
+
+/// The batch `delete` appends for `records` (tombstones and marker resets, §20.2, §20.3). Every
+/// key names a key of the store as it was before the batch (§20.2.7): one whose final stripping
+/// was `strip` and whose live keys `live` tells. A tombstone a reader would strip is written
+/// with stripping off around it (§9.6); a reset of `#_strip_trailing_whites_#` in the batch
+/// turns stripping back on (§10.3, §12.1).
+pub fn delete_records(
+    records: &[Record],
+    delimiter: u8,
+    strip: bool,
+    live: impl Fn(&str) -> bool,
+) -> Vec<Record> {
+    let set_strip = |value: &str| Record::Marker {
+        key: "#_strip_trailing_whites_#".into(),
+        values: vec![value.into()],
+    };
+    let mut batch = Vec::with_capacity(records.len() + 2);
+    let mut stripping = strip;
+    let mut guarded = false;
+    for record in records {
+        let Record::Tombstone { key } = record else {
+            if let Record::Marker { key, values } = record {
+                if values.is_empty() && key.eq_ignore_ascii_case("#_strip_trailing_whites_#") {
+                    stripping = true;
+                    guarded = false;
+                }
+            }
+            batch.push(record.clone());
+            continue;
+        };
+        let key = name_key(key, delimiter, strip, &live).into_owned();
+        if stripping && !guarded && strippable(&key, delimiter) {
+            batch.push(set_strip("false"));
+            guarded = true;
+        }
+        batch.push(Record::Tombstone { key });
+    }
+    if guarded {
+        batch.push(set_strip("true"));
+    }
+    batch
+}
+
 /// Records read from bulk input in `format` (§20.3.1, §21.7.2): a line is a record; a marker
 /// key is a marker line; a comment, an empty line or an empty key is skipped; a key alone is a
 /// tombstone. With `keys_only` (`delete -`) only the first field counts. An unterminated last
```

Apply:

```diff
--- a/src/cli/write.rs
+++ b/src/cli/write.rs
@@ -34,8 +34,10 @@ pub(crate) fn set(ctx: &mut Ctx) -> Result<i32, Error> {
 }
 
 /// `delete` (§20.2): one tombstone per key (a marker key is reset), or the keys on standard input.
+/// A key that a reader would strip names a key of the store (§20.2.7), so the store is read
+/// first; when it cannot be read whole, nothing is written and the exit status is 1.
 pub(crate) fn delete(ctx: &mut Ctx) -> Result<i32, Error> {
-    let records = if ctx.args.args == ["-"] {
+    let mut records = if ctx.args.args == ["-"] {
         bulk(ctx, true)?
     } else {
         ctx.args
@@ -44,6 +46,25 @@ pub(crate) fn delete(ctx: &mut Ctx) -> Result<i32, Error> {
             .map(|k| Record::from_args(k, &[]))
             .collect()
     };
+    let d = ctx.format.delimiter;
+    if ctx.format.strict && ops::delete_needs_store(&records, d) {
+        records = match ctx.open(Scope::KeysOnly, false) {
+            Ok(store) if store.incomplete() => {
+                diag!(
+                    "tsvz: {}: could not read every part of the store; nothing was written",
+                    ctx.store.display()
+                );
+                return Ok(1);
+            }
+            Ok(store) => {
+                let strip = store.markers().strip;
+                ops::delete_records(&records, d, strip, |k| store.get(k).is_some())
+            }
+            // A store delete creates starts with stripping on (§10.3).
+            Err(Error::Missing(_)) => ops::delete_records(&records, d, true, |_| false),
+            Err(e) => return Err(e),
+        };
+    }
     writer(ctx, None)?.append(&records)?;
     ctx.info(&format!("appended {} tombstones", records.len()));
     Ok(0)
```

- [ ] **Step 4: Run the tests to see them pass**

Run: `cargo test --all-features 2>&1 | grep -E '^test result|FAILED|panicked'; cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -1`
Expected: 162 passed, 0 failed; no clippy warning.

- [ ] **Step 5: Check conformance against TSVZ 4.7**

`../TSVZ` must be at TSVZ `main` with 4.7 or later (`grep -c '^20.2.7 \*\*Naming a key' ../TSVZ/tsvz-spec-v1.md` prints 1).

```bash
cargo build --release
python3 ../TSVZ/tsvzconform.py --impl "$PWD/target/release/tsvz" --class reader,writer,cli --feature bzip2,xz,zstd,crc32,crc32c,sha256,blake3,file-first | tail -3
```

Expected: `conformant: reader ✓   writer ✓   cli ✓`, with cli 96 passed (the 11 `20.2.7-*` cases among them). Before this task the `20.2.7-*` cases failed.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "Name a key as §20.2.7 says: get, has, pop, setdefault and delete name the live key equal to a KEY a reader would strip; delete names its keys against the store before it, guards its tombstones, and writes nothing when it cannot read the store.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_0198xw4TRnucPbiyi9csx4uj"
```

---

### Task 4: The protocol (`handler::protocol`)

Request lines are parsed into options, an operation and §13-decoded arguments, keeping each argument as written so a marker key (`#_defaults_#`) is told from a data key written `<#>_defaults_#` (§21.4.3). Responses are `#!` diagnostics, output lines and one status line (§21.6). Lines are read up to `\n`, dropping one `\r`, with a 64 MiB limit (§21.10.2).

**Files:**
- Create: `src/handler/mod.rs`, `src/handler/protocol.rs`
- Modify: `src/lib.rs`
- Test: `src/handler/protocol.rs` (unit tests)

**Interfaces:**
- Consumes: `ops::check_args`, `escape::{encode, encode_into, decode, is_marker_key}`.
- Produces (`crate::handler::protocol`, `#[doc(hidden)]`): `const LINE_LIMIT: usize = 64 << 20`; `const OPERATIONS: &[&str]`; `struct Request { sync: bool, written: bool, op: String, args: Vec<String>, raw: Vec<String> }` with `fn bulk(&self) -> bool`; `fn parse(line: &str) -> Result<Request, String>`; `fn field(value: &str, key: bool) -> String`; `fn decode(field: &str) -> String`; `fn decode_fields(line: &str) -> Vec<String>`; `pub struct Response { status: i32, lines: Vec<String>, diagnostics: Vec<String>, message: String }` (public, re-exported as `tsvz::handler::Response`) with `fn status(i32)`, `fn failed(i32, impl Into<String>)`, `fn encode(&self) -> Vec<u8>`; `fn read_line(input: &mut impl BufRead, limit: usize) -> io::Result<Option<String>>`; `fn read_response(input: &mut impl BufRead) -> io::Result<Response>`.

- [ ] **Step 1: Write the failing tests**

Apply:

```diff
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -19,6 +19,7 @@ mod error;
 pub mod escape;
 #[doc(hidden)]
 pub mod frame;
+pub mod handler;
 #[doc(hidden)]
 pub mod lock;
 pub mod maint;
```

Create `src/handler/mod.rs`:

```rust
//! The write handler of §21: one process serves a store over a Unix-domain socket, so that many
//! producers and command lines share one loaded store and one appender.

#[doc(hidden)]
pub mod protocol;

pub use protocol::Response;
```

Create `src/handler/protocol.rs` holding only its test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn requests_have_options_an_operation_and_decoded_arguments() {
        let r = parse("--sync\t--x-written\tset\t<#>k\ta<sep>b\t").unwrap();
        assert!(r.sync && r.written);
        assert_eq!(r.op, "set");
        assert_eq!(r.args, strings(&["#k", "a\tb", ""]));
        assert_eq!(r.raw, strings(&["<#>k", "a<sep>b", ""]));
        assert!(!r.bulk());
        let m = parse("set\t#_defaults_#\td1").unwrap();
        assert!(!m.sync && !m.written);
        assert_eq!(m.raw[0], "#_defaults_#");
        assert!(parse("set\t-").unwrap().bulk());
        assert!(parse("delete\t-").unwrap().bulk());
        assert!(!parse("get\t-").unwrap().bulk());
        assert!(parse("stop").is_ok() && parse("popitem\tfirst").is_ok());
    }

    #[test]
    fn bad_requests_are_usage_errors() {
        for (line, want) in [
            ("", "missing operation"),
            ("--sync", "missing operation"),
            ("--bogus\tread", "unknown option --bogus"),
            ("serve", "unknown operation serve"),
            ("frobnicate\tk", "unknown operation"),
            ("get", "needs a KEY"),
            ("has", "needs a KEY"),
            ("get\ta\t", "must not be empty"),
            ("len\tx", "takes no arguments"),
            ("popitem\tmiddle", "first or last"),
            ("setdefault\t#_defaults_#\tx", "marker"),
        ] {
            let err = parse(line).unwrap_err();
            assert!(err.contains(want), "{line:?}: {err}");
        }
        // A data key written <#>… is not a marker key (§21.4.3).
        assert!(parse("setdefault\t<#>_defaults_#\tx").is_ok());
    }

    #[test]
    fn responses_encode_diagnostics_output_and_a_status_line() {
        let r = Response {
            status: 3,
            lines: strings(&["<#>k\tv", "2"]),
            diagnostics: strings(&["a\tb", "c\nd"]),
            message: "why\tnot".into(),
        };
        assert_eq!(
            String::from_utf8(r.encode()).unwrap(),
            "#!\ta<sep>b\n#!\tc<LF>d\n<#>k\tv\n2\n#3\twhy<sep>not\n"
        );
        assert_eq!(Response::status(0).encode(), b"#0\n");
        assert_eq!(Response::failed(2, "no").encode(), b"#2\tno\n");
    }

    #[test]
    fn responses_read_back_as_written() {
        let r = Response {
            status: 1,
            lines: strings(&["a\tx", "<#>h"]),
            diagnostics: strings(&["warn\tme"]),
            message: "could not read every part".into(),
        };
        let mut input = Cursor::new(r.encode());
        assert_eq!(read_response(&mut input).unwrap(), r);
        let mut cut = Cursor::new(b"a\tx\n".to_vec());
        assert_eq!(
            read_response(&mut cut).unwrap_err().kind(),
            std::io::ErrorKind::UnexpectedEof
        );
        let mut odd = Cursor::new(b"#x\n".to_vec());
        assert_eq!(read_response(&mut odd).unwrap().status, 1);
    }

    #[test]
    fn lines_end_at_newline_drop_one_cr_and_respect_the_limit() {
        let mut input = Cursor::new(b"a\r\r\nb\r\nc".to_vec());
        assert_eq!(read_line(&mut input, 10).unwrap().as_deref(), Some("a\r"));
        assert_eq!(read_line(&mut input, 10).unwrap().as_deref(), Some("b"));
        assert_eq!(
            read_line(&mut input, 10).unwrap(),
            None,
            "no \\n: not a line"
        );
        let mut long = Cursor::new(b"0123456789AB\n".to_vec());
        assert_eq!(
            read_line(&mut long, 10).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
        let mut exact = Cursor::new(b"0123456789\n".to_vec());
        assert_eq!(read_line(&mut exact, 10).unwrap().unwrap().len(), 10);
        let mut bad = Cursor::new(b"\xff\n".to_vec());
        assert_eq!(
            read_line(&mut bad, 10).unwrap().as_deref(),
            Some("\u{fffd}")
        );
    }

    #[test]
    fn fields_use_section_13_with_tab() {
        assert_eq!(field("#k\t", true), "<#>k<sep>");
        assert_eq!(field("#k", false), "#k");
        assert_eq!(decode_fields("<#>k\ta<sep>b"), strings(&["#k", "a\tb"]));
    }
}
```

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test --all-features --lib handler:: 2>&1 | grep -E '^error' | sort | uniq -c | head -5`
Expected: compile errors: `cannot find function parse`, `field`, `decode_fields`, `cannot find struct Response`.

- [ ] **Step 3: Implement the protocol**

Put this above the test module in `src/handler/protocol.rs`:

```rust
//! Request and response lines of the handler protocol (§21.4–§21.6, §21.10).

use std::io::{self, BufRead, Read};

use crate::escape::{self, is_marker_key};
use crate::ops::check_args;

/// A line longer than this closes the connection (§21.10.2 asks for at least 1 MiB; TSVZ.py's
/// limit).
pub const LINE_LIMIT: usize = 64 << 20;

/// The operations a handler answers: those of §20.2 except `serve`, and `stop` (§21.7.1).
pub const OPERATIONS: &[&str] = &[
    "read",
    "get",
    "set",
    "append",
    "delete",
    "clear",
    "scrub",
    "verify",
    "parts",
    "has",
    "len",
    "keys",
    "pop",
    "popitem",
    "setdefault",
    "stop",
];

/// A parsed request (§21.5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// `--sync`: acknowledge writes once they are on durable storage (§21.5.2).
    pub sync: bool,
    /// `--x-written` (TSVZ.py's extension): acknowledge writes once written, and answer status 1
    /// when the write failed.
    pub written: bool,
    pub op: String,
    /// The arguments, §13-decoded.
    pub args: Vec<String>,
    /// The arguments as written: a marker key is told from a data key by how it is written
    /// (§21.4.3).
    pub raw: Vec<String>,
}

impl Request {
    /// Whether record lines up to a `#` line follow this request (§21.7.2).
    pub fn bulk(&self) -> bool {
        matches!(self.op.as_str(), "set" | "append" | "delete") && self.raw == ["-"]
    }
}

/// Parse a request line, without its line end (§21.5). The error is the message of a usage
/// error: status 2, and the connection stays usable (§21.10.1).
pub fn parse(line: &str) -> Result<Request, String> {
    let fields: Vec<&str> = line.split('\t').collect();
    let (mut sync, mut written) = (false, false);
    let mut i = 0;
    while i < fields.len() && fields[i].starts_with("--") {
        match fields[i] {
            "--sync" => sync = true,
            "--x-written" => written = true,
            other => return Err(format!("unknown option {other}")),
        }
        i += 1;
    }
    let op = fields.get(i).copied().unwrap_or("");
    if op.is_empty() {
        return Err("missing operation".into());
    }
    if !OPERATIONS.contains(&op) {
        return Err(format!("unknown operation {op}"));
    }
    let raw: Vec<String> = fields[i + 1..].iter().map(|f| f.to_string()).collect();
    let args: Vec<String> = raw.iter().map(|f| decode(f)).collect();
    check_args(op, &args)?;
    if op == "setdefault" && is_marker_key(&raw[0]) {
        return Err("setdefault takes a data KEY, not a marker key (§21.7.3)".into());
    }
    Ok(Request {
        sync,
        written,
        op: op.to_string(),
        args,
        raw,
    })
}

/// A field of the protocol: §13 with TAB whatever the store's variant (§21.4.2); with `key`, a
/// leading `#` is written `<#>`.
pub fn field(value: &str, key: bool) -> String {
    escape::encode(value, b'\t', key)
}

/// A protocol field, decoded (§21.4.3).
pub fn decode(field: &str) -> String {
    escape::decode(field, b'\t').into_owned()
}

/// The decoded fields of an output or record line.
pub fn decode_fields(line: &str) -> Vec<String> {
    line.split('\t').map(decode).collect()
}

/// A response (§21.6).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Response {
    /// The exit status of §20.6.
    pub status: i32,
    /// The output lines as sent: rows as `read --format records` prints them in the TSVZ
    /// variant, keys, the count of `len`, `verify` and `parts` fields (§21.6.1).
    pub lines: Vec<String>,
    /// The diagnostics (`#!` lines), decoded.
    pub diagnostics: Vec<String>,
    /// The status line's message, decoded; empty when it has none.
    pub message: String,
}

impl Response {
    /// A response with no output and no message.
    pub fn status(status: i32) -> Response {
        Response {
            status,
            ..Response::default()
        }
    }

    /// A response with no output and a message.
    pub fn failed(status: i32, message: impl Into<String>) -> Response {
        Response {
            status,
            message: message.into(),
            ..Response::default()
        }
    }

    /// The response as sent: diagnostic lines, output lines, then the status line.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = String::new();
        for text in &self.diagnostics {
            out.push_str("#!\t");
            escape::encode_into(&mut out, text, b'\t', false);
            out.push('\n');
        }
        for line in &self.lines {
            out.push_str(line);
            out.push('\n');
        }
        out.push('#');
        out.push_str(&self.status.to_string());
        if !self.message.is_empty() {
            out.push('\t');
            escape::encode_into(&mut out, &self.message, b'\t', false);
        }
        out.push('\n');
        out.into_bytes()
    }
}

/// One line without its `\n` and one `\r` before it, invalid UTF-8 replaced (§21.4.1). None
/// at the end of the input, including a last line with no `\n`: it is not a whole line. A line
/// longer than `limit` bytes is an error (`InvalidData`).
pub fn read_line(input: &mut impl BufRead, limit: usize) -> io::Result<Option<String>> {
    let mut buf = Vec::new();
    input
        .by_ref()
        .take(limit as u64 + 2)
        .read_until(b'\n', &mut buf)?;
    if buf.last() != Some(&b'\n') {
        if buf.len() > limit {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "line too long"));
        }
        return Ok(None);
    }
    buf.pop();
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    if buf.len() > limit {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "line too long"));
    }
    Ok(Some(match String::from_utf8(buf) {
        Ok(text) => text,
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    }))
}

/// Read one response: lines up to its status line (§21.6). The connection ending first is
/// `UnexpectedEof`; a status that is not a number reads as 1.
pub fn read_response(input: &mut impl BufRead) -> io::Result<Response> {
    let mut response = Response::default();
    loop {
        let Some(line) = read_line(input, LINE_LIMIT)? else {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the handler closed the connection before its answer ended",
            ));
        };
        if let Some(text) = line.strip_prefix("#!") {
            response
                .diagnostics
                .push(decode(text.strip_prefix('\t').unwrap_or(text)));
        } else if let Some(status) = line.strip_prefix('#') {
            let (number, message) = status.split_once('\t').unwrap_or((status, ""));
            let digits = !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit());
            response.status = if digits {
                number.parse().unwrap_or(1)
            } else {
                1
            };
            response.message = decode(message);
            return Ok(response);
        } else {
            response.lines.push(line);
        }
    }
}
```

- [ ] **Step 4: Run the tests to see them pass**

Run: `cargo test --all-features 2>&1 | grep -E '^test result|FAILED|panicked'; cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -1`
Expected: the 6 protocol tests pass; 168 passed in total, 0 failed; no clippy warning.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "handler::protocol: request and response lines (§21.4-§21.6, §21.10).

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_0198xw4TRnucPbiyi9csx4uj"
```

---

### Task 5: The engine (`handler::engine`)

One `Engine` per served store. Its state follows the files: a request that reads first waits for every queued write (`barrier`), then `Store::refresh`es, so it sees acknowledged writes and other writers' records (§21.8.1) and answers what a reader of the files returns (§21.8.5). Writes are encoded once and queued; one writer thread appends everything queued as one batch through `Writer::append_payload` (one active-part lookup, one lock shared with TSVZ.py, one `write`, one `fsync` when any entry asked for one). Requests are serialized by one mutex; acknowledgements (§21.9) are waited for outside it. Warnings raised while a request holds the mutex become its `#!` lines. A `delete` that must name a key (§20.2.7, Task 3) reads the store under the mutex, so naming and writing are one atomic step (§21.8.2); when that read is incomplete it answers 1, "could not read every part of the store; nothing was written", as TSVZ.py's handler does.

**Files:**
- Create: `src/handler/engine.rs`
- Modify: `src/handler/mod.rs`
- Test: `src/handler/engine.rs` (unit tests: the engine in process, no socket)

**Interfaces:**
- Consumes: `protocol::{Request, Response}`, `ops::*` (including `delete_needs_store`, `delete_records`), `maint::{clear, scrub, Outcome}`, `Store::{open, refresh, markers, incomplete}`, `Writer::{open, member_size, fsync, warnings, append_payload}`, `write::encode_record`, `parts::{list, active}`.
- Produces (`crate::handler::engine`, `#[doc(hidden)]`): `const NO_STORE: &str = "no such store"`; `const INCOMPLETE: &str = "could not read every part of the store"`; `struct Engine` with `fn open(path: &Path, log: Option<WarnFn>) -> Result<Engine, Error>` (refuses a loose store; creates a missing one), `fn path(&self) -> &Path`, `fn handle(&self, req: &Request, bulk: Option<&[String]>) -> Response`, `fn flush(&self) -> Result<(), Error>`, `fn close(&self)` (idempotent; also on drop). `Engine` is `Send + Sync`.

- [ ] **Step 1: Write the failing tests**

Apply:

```diff
--- a/src/handler/mod.rs
+++ b/src/handler/mod.rs
@@ -1,6 +1,8 @@
 //! The write handler of §21: one process serves a store over a Unix-domain socket, so that many
 //! producers and command lines share one loaded store and one appender.
 
+#[doc(hidden)]
+pub mod engine;
 #[doc(hidden)]
 pub mod protocol;
 
```

Create `src/handler/engine.rs` holding only its test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    use crate::handler::protocol::parse;

    fn engine(dir: &Path, name: &str) -> Engine {
        Engine::open(&dir.join(name), None).unwrap()
    }

    fn ask(e: &Engine, line: &str) -> Response {
        e.handle(&parse(line).unwrap(), None)
    }

    fn bulk(e: &Engine, line: &str, records: &[&str]) -> Response {
        let lines: Vec<String> = records.iter().map(|r| r.to_string()).collect();
        e.handle(&parse(line).unwrap(), Some(&lines))
    }

    fn lines(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn append(p: &Path, data: &[u8]) {
        fs::OpenOptions::new()
            .append(true)
            .open(p)
            .unwrap()
            .write_all(data)
            .unwrap();
    }

    #[test]
    fn a_part_rewritten_in_place_then_grown_past_its_old_size_is_read_again() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(&p, "a\t1\nb\t2\n").unwrap();
        let e = engine(dir.path(), "s.tsvz");
        assert_eq!(ask(&e, "len").lines, ["2"]);
        // Another tool rewrites the part in place (§19.8), then appends past its old size: the
        // same inode, and larger, but not an append.
        fs::write(&p, "c\t3\n").unwrap();
        append(&p, b"d\t4\ne\t5\n");
        assert_eq!(ask(&e, "keys").lines, ["c", "d", "e"]);
    }

    #[test]
    fn a_missing_store_is_created_and_a_removed_one_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.csvz");
        let e = engine(dir.path(), "s.csvz");
        assert_eq!(fs::read(&p).unwrap(), b"");
        assert_eq!(ask(&e, "len").lines, ["0"]);
        fs::remove_file(&p).unwrap();
        let r = ask(&e, "len");
        assert_eq!((r.status, r.message.as_str()), (1, "no such store"));
        assert_eq!(ask(&e, "--x-written\tset\tk\tv,w").status, 0);
        // The protocol is TAB-delimited whatever the store's variant (§21.4.2).
        assert_eq!(ask(&e, "read").lines, ["k\tv,w"]);
        assert_eq!(fs::read_to_string(&p).unwrap(), "k,v<sep>w\n");
        assert!(Engine::open(&dir.path().join("s.tsv"), None).is_err());
    }

    #[test]
    fn reads_see_acknowledged_writes_and_other_writers() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(&p, "a\tx\n").unwrap();
        let e = engine(dir.path(), "s.tsvz");
        assert_eq!(ask(&e, "set\tb\ty").status, 0);
        assert_eq!(ask(&e, "get\tb").lines, ["b\ty"]);
        append(&p, b"c\tz\n");
        fs::write(dir.path().join("s.tsvz.1"), "d\tw\n").unwrap();
        assert_eq!(ask(&e, "keys").lines, ["a", "b", "c", "d"]);
        // The handler appends to the last part (§17.7, §21.8.4).
        assert_eq!(ask(&e, "--x-written\tdelete\ta").status, 0);
        let last = fs::read_to_string(dir.path().join("s.tsvz.1")).unwrap();
        assert_eq!(last, "d\tw\na\n");
        let r = ask(&e, "get\ta\tzz");
        assert_eq!((r.status, r.lines.len()), (3, 2), "{r:?}");
        assert_eq!((ask(&e, "has\tb").status, ask(&e, "has\ta").status), (0, 3));
    }

    #[test]
    fn marker_keys_and_data_keys_are_told_apart_by_how_they_are_written() {
        let dir = tempfile::tempdir().unwrap();
        let e = engine(dir.path(), "s.tsvz");
        ask(&e, "set\t#_defaults_#\td1\td2");
        ask(&e, "set\t<#>_defaults_#\tq");
        let r = ask(&e, "get\tm");
        assert_eq!((r.status, r.lines), (3, lines(&["m\td1\td2"])));
        assert_eq!(
            ask(&e, "get\t<#>_defaults_#").lines,
            ["<#>_defaults_#\tq\td2"]
        );
        let r = ask(&e, "read");
        assert_eq!(r.lines, ["<#>_defaults_#\tq\td2"]);
    }

    #[test]
    fn bulk_requests_are_one_batch() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        let e = engine(dir.path(), "s.tsvz");
        let r = bulk(
            &e,
            "--x-written\tset\t-",
            &["a\t1", "#comment", "#_defaults_#\td", "b\t", "c\t3", "c"],
        );
        assert_eq!(r.status, 0, "{r:?}");
        assert_eq!(
            fs::read_to_string(&p).unwrap(),
            "a\t1\n#_defaults_#\td\nb\t\nc\t3\nc\n"
        );
        assert_eq!(ask(&e, "read").lines, ["a\t1", "b\t"]);
        assert_eq!(bulk(&e, "delete\t-", &["a\tignored", "zz"]).status, 0);
        assert_eq!(ask(&e, "keys").lines, ["b"]);
        assert_eq!(bulk(&e, "set\t-", &[]).status, 0);
    }

    #[test]
    fn pop_popitem_and_setdefault_answer_rows() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(&p, "a\t1\t2\nb\t3\nc\t4\n").unwrap();
        let e = engine(dir.path(), "s.tsvz");
        let r = ask(&e, "pop\tb");
        assert_eq!((r.status, r.lines), (0, lines(&["b\t3\t"])));
        assert!(
            fs::read_to_string(&p).unwrap().ends_with("\nb\n"),
            "written before the answer"
        );
        assert_eq!(ask(&e, "pop\tb").status, 3);
        assert_eq!(ask(&e, "popitem\tfirst").lines, ["a\t1\t2"]);
        assert_eq!(ask(&e, "popitem").lines, ["c\t4\t"]);
        assert_eq!(ask(&e, "popitem").status, 3);
        let r = ask(&e, "setdefault\tn\tv");
        assert_eq!((r.status, r.lines), (0, lines(&["n\tv\t"])));
        assert_eq!(ask(&e, "setdefault\tn\tw").lines, ["n\tv\t"]);
    }

    #[test]
    fn concurrent_pops_of_one_key_have_one_winner() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("s.tsvz"), "k\tv\n").unwrap();
        let e = Arc::new(engine(dir.path(), "s.tsvz"));
        let wins: usize = (0..8)
            .map(|_| {
                let e = e.clone();
                std::thread::spawn(move || ask(&e, "pop\tk").status == 0)
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|t| t.join().unwrap() as usize)
            .sum();
        assert_eq!(wins, 1);
    }

    #[test]
    fn a_disk_acknowledged_or_synced_write_is_in_the_file_when_answered() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        let e = engine(dir.path(), "s.tsvz");
        assert_eq!(ask(&e, "--sync\tset\ta\t1").status, 0);
        assert_eq!(fs::read_to_string(&p).unwrap(), "a\t1\n");
        assert_eq!(ask(&e, "--sync\tset\t#_write_ack_#\tdisk").status, 0);
        assert_eq!(ask(&e, "len").lines, ["1"], "the marker is read");
        assert_eq!(ask(&e, "set\tb\t2").status, 0);
        assert!(fs::read_to_string(&p).unwrap().ends_with("b\t2\n"));
    }

    #[test]
    fn failed_writes_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(&p, "a\t1\n").unwrap();
        let e = engine(dir.path(), "s.tsvz");
        assert_eq!(ask(&e, "len").lines, ["1"]);
        // A directory where the part was: every append fails, even as root.
        fs::remove_file(&p).unwrap();
        fs::create_dir(&p).unwrap();
        let r = ask(&e, "--x-written\tset\tb\t2");
        assert_eq!(r.status, 1, "{r:?}");
        assert!(
            r.diagnostics
                .iter()
                .any(|d| d.contains("writes are failing")),
            "{r:?}"
        );
        assert_eq!(
            ask(&e, "set\tc\t3").status,
            0,
            "acknowledged before it is written"
        );
        let r = ask(&e, "pop\ta");
        assert_eq!((r.status, r.lines.len()), (1, 0), "{r:?}");
        fs::remove_dir(&p).unwrap();
        fs::write(&p, "a\t1\n").unwrap();
        assert_eq!(ask(&e, "--x-written\tset\td\t4").status, 0);
        let r = ask(&e, "len");
        assert!(r.diagnostics.is_empty(), "writes work again: {r:?}");
    }

    #[test]
    fn an_incomplete_read_answers_1_with_what_was_read_and_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(&p, "a\t1\n").unwrap();
        fs::create_dir(dir.path().join("s.tsvz.1")).unwrap();
        let last = dir.path().join("s.tsvz.2");
        fs::write(&last, "b\t2\n").unwrap();
        let e = engine(dir.path(), "s.tsvz");
        let r = ask(&e, "read");
        assert_eq!((r.status, r.lines.clone()), (1, lines(&["a\t1", "b\t2"])));
        assert!(
            r.diagnostics.iter().any(|d| d.contains("s.tsvz.1")),
            "{r:?}"
        );
        assert_eq!(ask(&e, "len").status, 1);
        for change in ["pop\tb", "popitem", "setdefault\tn\tv", "clear"] {
            let r = ask(&e, change);
            assert_eq!((r.status, r.lines.len()), (1, 0), "{change}: {r:?}");
        }
        assert_eq!(fs::read_to_string(&last).unwrap(), "b\t2\n");
        let stopped = dir.path().join("t.tsvz");
        fs::write(&stopped, "#_on_corruption_#\tstop\na\t1\n\0\0c\t3\nd\t4\n").unwrap();
        let e = engine(dir.path(), "t.tsvz");
        let r = ask(&e, "get\ta");
        assert_eq!((r.status, r.lines), (1, lines(&["a\t1"])));
        assert_eq!(ask(&e, "verify").status, 1);
    }

    #[test]
    fn clear_scrub_verify_and_parts() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(
            &p,
            "#_checksum_crc32_#\na\tx\n#_checksum_crc32_#\t00000000\n",
        )
        .unwrap();
        let e = engine(dir.path(), "s.tsvz");
        let r = ask(&e, "verify");
        assert_eq!((r.status, r.lines.len()), (4, 1), "{r:?}");
        assert_eq!(ask(&e, "scrub").status, 0);
        assert_eq!(ask(&e, "verify").status, 0);
        assert_eq!(ask(&e, "read").lines, ["a\tx"]);
        assert_eq!(ask(&e, "clear").status, 0);
        assert_eq!(ask(&e, "len").lines, ["0"]);
        fs::write(dir.path().join("s.tsvz.1"), "b\t2\n").unwrap();
        let r = ask(&e, "scrub");
        assert_eq!(r.status, 1, "a multi-part store is refused: {r:?}");
        assert_eq!(ask(&e, "get\tb").lines, ["b\t2"]);
        let r = ask(&e, "parts");
        assert_eq!(r.lines.len(), 2);
        assert!(r.lines[1].ends_with("s.tsvz.1\tactive"), "{r:?}");
    }

    #[test]
    fn diagnostics_go_with_the_request_that_raised_them() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(&p, "a\t1\n").unwrap();
        let e = engine(dir.path(), "s.tsvz");
        assert!(ask(&e, "len").diagnostics.is_empty());
        append(&p, b"b\t2\ntorn");
        let r = ask(&e, "len");
        assert_eq!(r.lines, ["2"]);
        assert!(
            r.diagnostics.iter().any(|d| d.contains("uncommitted")),
            "{r:?}"
        );
        assert!(ask(&e, "len").diagnostics.is_empty(), "said once");
    }

    #[test]
    fn closing_writes_everything_queued() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        let e = engine(dir.path(), "s.tsvz");
        for i in 0..100 {
            assert_eq!(ask(&e, &format!("set\tk{i}\tv")).status, 0);
        }
        e.close();
        assert_eq!(fs::read_to_string(&p).unwrap().lines().count(), 100);
        let r = ask(&e, "len");
        assert_eq!(r.status, 1, "a closed engine answers no more: {r:?}");
    }

    #[test]
    fn delete_names_keys_under_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        let both =
            "#_strip_trailing_whites_#\tfalse\nk \tv1\n#_strip_trailing_whites_#\ttrue\nk\tv2\n";
        fs::write(&p, both).unwrap();
        let e = engine(dir.path(), "s.tsvz");
        assert_eq!(ask(&e, "delete\tk ").status, 0);
        assert_eq!(ask(&e, "keys").lines, ["k"]);
        bulk(
            &e,
            "set\t-",
            &[
                "#_strip_trailing_whites_#\tfalse",
                "m \tv3",
                "#_strip_trailing_whites_#\ttrue",
            ],
        );
        assert_eq!(bulk(&e, "delete\t-", &["m "]).status, 0);
        assert_eq!(ask(&e, "read").lines, ["k\tv2"]);
        let stopped = dir.path().join("t.tsvz");
        fs::write(&stopped, "#_on_corruption_#\tstop\nk\t1\n\0\0c\t3\n").unwrap();
        let e = engine(dir.path(), "t.tsvz");
        let r = ask(&e, "--x-written\tdelete\tk ");
        assert_eq!(r.status, 1, "{r:?}");
        assert!(r.message.contains("nothing was written"), "{r:?}");
        assert_eq!(
            fs::read_to_string(&stopped).unwrap(),
            "#_on_corruption_#\tstop\nk\t1\n\0\0c\t3\n"
        );
    }
}
```

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test --all-features --lib handler::engine 2>&1 | grep -E '^error' | sort | uniq -c | head -5`
Expected: compile errors: `cannot find type Engine`, `cannot find struct Response` in the test module's scope.

- [ ] **Step 3: Implement the engine**

Put this above the test module in `src/handler/engine.rs`:

```rust
//! One store's live state for its handler (§21.7–§21.9).
//!
//! The state follows the files: every write is appended by one writer thread and comes back
//! through `Store::refresh`, like any other writer's (§21.1.2). A request that reads waits until
//! every queued write is written, then refreshes, so it sees every write acknowledged before it
//! and every record committed to the files before it (§21.8.1), and what it answers is what a
//! reader of the files returns (§21.8.5). Requests are serialized by one mutex; acknowledgements
//! are waited for outside it.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;

use crate::handler::protocol::{Request, Response};
use crate::maint::{self, Outcome};
use crate::name::{self, Format};
use crate::ops::{self, Out, Plan};
use crate::write::{Record, Writer, encode_record};
use crate::{Error, Options, Scope, Store, WarnFn, Warning, escape, parts};

/// The message of a read operation on a store that does not exist (§20.2.2).
pub const NO_STORE: &str = "no such store";
/// The message of a read that skipped a part or stopped at damaged data (§21.8.5).
pub const INCOMPLETE: &str = "could not read every part of the store";
/// Batch failures remembered for the requests that wait on them.
const FAILURES: usize = 64;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

struct Entry {
    seq: u64,
    payload: Vec<u8>,
    durable: bool,
}

#[derive(Default)]
struct Queue {
    entries: Vec<Entry>,
    member_size: u64,
    submitted: u64,
    written: u64,
    durable: u64,
    /// (first seq, last seq, error) of failed batches.
    failures: VecDeque<(u64, u64, String)>,
    /// The last batch's error, until a batch is written again.
    failing: Option<String>,
    unsynced: bool,
    closing: bool,
}

/// The single appender of the store (§18.3): each turn appends everything queued as one batch.
struct Batcher {
    store: PathBuf,
    queue: Mutex<Queue>,
    changed: Condvar,
    log: Option<WarnFn>,
}

impl Batcher {
    /// Queue `payload` (records encoded for the store, each ending in `\n`); its sequence number.
    fn submit(&self, payload: Vec<u8>, durable: bool, member_size: u64) -> u64 {
        let mut q = lock(&self.queue);
        q.submitted += 1;
        let seq = q.submitted;
        q.member_size = member_size;
        q.entries.push(Entry {
            seq,
            payload,
            durable,
        });
        self.changed.notify_all();
        seq
    }

    /// Wait until entry `seq` is written (`durable`: and fsync'd); its batch's error, if any.
    fn wait(&self, seq: u64, durable: bool) -> Result<(), String> {
        let mut q = lock(&self.queue);
        while (if durable { q.durable } else { q.written }) < seq {
            q = self.changed.wait(q).unwrap_or_else(|e| e.into_inner());
        }
        match q.failures.iter().find(|f| f.0 <= seq && seq <= f.1) {
            Some(f) => Err(f.2.clone()),
            None => Ok(()),
        }
    }

    /// Wait until everything queued so far is written (§21.8.1).
    fn barrier(&self) {
        let mut q = lock(&self.queue);
        while q.written < q.submitted {
            q = self.changed.wait(q).unwrap_or_else(|e| e.into_inner());
        }
    }

    /// Wait until everything queued is written, then fsync what was not (§21.9.3).
    fn flush(&self) -> Result<(), String> {
        self.barrier();
        let mut q = lock(&self.queue);
        if q.unsynced {
            self.fsync().map_err(|e| e.to_string())?;
            q.unsynced = false;
            q.durable = q.written;
            self.changed.notify_all();
        }
        Ok(())
    }

    fn failing(&self) -> Option<String> {
        lock(&self.queue).failing.clone()
    }

    fn fsync(&self) -> Result<(), Error> {
        let part = parts::active(&self.store)?;
        match std::fs::OpenOptions::new().append(true).open(&part.path) {
            Ok(file) => file.sync_data().map_err(|e| Error::io(&part.path, e)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(Error::io(&part.path, e)),
        }
    }

    fn run(&self) {
        loop {
            let (batch, member_size) = {
                let mut q = lock(&self.queue);
                while q.entries.is_empty() && !q.closing {
                    q = self.changed.wait(q).unwrap_or_else(|e| e.into_inner());
                }
                if q.entries.is_empty() {
                    break;
                }
                (std::mem::take(&mut q.entries), q.member_size)
            };
            let durable = batch.iter().any(|e| e.durable);
            let (first, last) = (batch[0].seq, batch[batch.len() - 1].seq);
            let mut payload = Vec::with_capacity(batch.iter().map(|e| e.payload.len()).sum());
            for e in &batch {
                payload.extend_from_slice(&e.payload);
            }
            let result = Writer::open(&self.store).and_then(|w| {
                w.member_size(member_size as usize)
                    .fsync(durable)
                    .warnings(self.log.clone())
                    .append_payload(&payload)
            });
            let mut q = lock(&self.queue);
            q.written = last;
            match result {
                Ok(()) => {
                    q.failing = None;
                    if durable {
                        q.durable = last;
                    } else {
                        q.unsynced = true;
                    }
                }
                Err(e) => {
                    let message = e.to_string();
                    if let Some(log) = &self.log {
                        log(&Warning {
                            part: self.store.clone(),
                            offset: None,
                            message: format!("a batch of {} writes failed: {message}", batch.len()),
                        });
                    }
                    q.durable = last;
                    q.failures.push_back((first, last, message.clone()));
                    if q.failures.len() > FAILURES {
                        q.failures.pop_front();
                    }
                    q.failing = Some(message);
                }
            }
            self.changed.notify_all();
        }
        let unsynced = lock(&self.queue).unsynced;
        if unsynced {
            if let Err(e) = self.fsync() {
                if let Some(log) = &self.log {
                    log(&Warning {
                        part: self.store.clone(),
                        offset: None,
                        message: format!("fsync failed: {e}"),
                    });
                }
            }
        }
    }
}

struct State {
    store: Option<Store>,
    /// Warnings raised while handling the current request: its `#!` lines.
    said: Arc<Mutex<Vec<String>>>,
    closed: bool,
}

/// Output lines of a response (§21.6.1): rows and keys as `read --format records` prints them
/// in the TSVZ variant, fields with a leading `#` written `<#>`.
#[derive(Default)]
struct Lines(Vec<String>);

impl Out for Lines {
    fn row<'a>(&mut self, key: &str, cells: impl Iterator<Item = &'a str>) -> Result<(), Error> {
        let mut line = String::new();
        ops::record_line(&mut line, key, cells, b'\t', true);
        line.pop();
        self.0.push(line);
        Ok(())
    }

    fn fields(&mut self, fields: &[&str]) -> Result<(), Error> {
        let mut line = String::new();
        ops::fields_line(&mut line, fields, true);
        line.pop();
        self.0.push(line);
        Ok(())
    }

    fn count(&mut self, n: usize) -> Result<(), Error> {
        self.0.push(n.to_string());
        Ok(())
    }
}

/// A row as a response line.
fn row_line(key: &str, cells: &[String]) -> String {
    let mut out = Lines::default();
    let _ = out.row(key, cells.iter().map(String::as_str));
    out.0.pop().unwrap_or_default()
}

/// The record a request's KEY and VALUEs write: a marker line when the key is written as a
/// marker key (§21.4.3), a tombstone when there are no values.
fn record(raw: &str, key: &str, values: &[String]) -> Record {
    if escape::is_marker_key(raw) {
        Record::Marker {
            key: raw.to_string(),
            values: values.to_vec(),
        }
    } else if values.is_empty() {
        Record::Tombstone {
            key: key.to_string(),
        }
    } else {
        Record::Data {
            key: key.to_string(),
            values: values.to_vec(),
        }
    }
}

/// A write queued by a request: wait for it as §21.9 says before answering.
struct Queued {
    seq: u64,
    durable: bool,
}

/// One store's handler state: the store as its files give it, and its writer thread.
pub struct Engine {
    path: PathBuf,
    format: Format,
    state: Mutex<State>,
    batcher: Arc<Batcher>,
    thread: Mutex<Option<JoinHandle<()>>>,
    log: Option<WarnFn>,
}

impl Engine {
    /// Serve the strict store at `path`, creating it when it is missing (§21.13.1). `log` gets
    /// every warning and failed batch, for `serve`'s standard error.
    pub fn open(path: &Path, log: Option<WarnFn>) -> Result<Engine, Error> {
        let format = name::format_of(path).ok_or_else(|| Error::BadPath(path.to_path_buf()))?;
        if !format.strict {
            return Err(Error::Refused(format!(
                "{}: serve takes a strict store (.tsvz .csvz .nsvz .psvz)",
                path.display()
            )));
        }
        if parts::list(path)?.is_empty() {
            Writer::open(path)?.append(&[])?;
        }
        let said = Arc::new(Mutex::new(Vec::new()));
        let store = Store::open(path, Engine::options(&said, &log))?;
        let batcher = Arc::new(Batcher {
            store: path.to_path_buf(),
            queue: Mutex::new(Queue::default()),
            changed: Condvar::new(),
            log: log.clone(),
        });
        let runner = batcher.clone();
        let thread = std::thread::Builder::new()
            .name("tsvz-writer".into())
            .spawn(move || runner.run())
            .map_err(|e| Error::io(path, e))?;
        Ok(Engine {
            path: path.to_path_buf(),
            format,
            state: Mutex::new(State {
                store: Some(store),
                said,
                closed: false,
            }),
            batcher,
            thread: Mutex::new(Some(thread)),
            log,
        })
    }

    /// How the engine reads: the whole store, past unreadable parts, warnings into `said`.
    fn options(said: &Arc<Mutex<Vec<String>>>, log: &Option<WarnFn>) -> Options {
        let (said, log) = (said.clone(), log.clone());
        Options {
            scope: Scope::All,
            verify: false,
            delimiter: None,
            warnings: Some(Arc::new(move |w: &Warning| {
                lock(&said).push(format!("{}: {}", w.part.display(), w.message));
                if let Some(log) = &log {
                    log(w);
                }
            })),
            skip_unreadable: true,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Run `f` on the state under the mutex; also return the warnings raised meanwhile.
    fn locked<R>(
        &self,
        f: impl FnOnce(&mut State) -> Result<R, Error>,
    ) -> (Result<R, Error>, Vec<String>) {
        let mut st = lock(&self.state);
        let result = if st.closed {
            Err(Error::Refused("the handler is stopping".into()))
        } else {
            f(&mut st)
        };
        let said = std::mem::take(&mut *lock(&st.said));
        (result, said)
    }

    /// Catch up with the files: wait for queued writes, then follow them (§21.8.1).
    fn fresh(&self, st: &mut State) -> Result<(), Error> {
        self.batcher.barrier();
        let reopened = match &mut st.store {
            Some(store) => match store.refresh() {
                Ok(_) => return Ok(()),
                Err(Error::Missing(_)) => None,
                Err(e) => return Err(e),
            },
            None => match Store::open(&self.path, Engine::options(&st.said, &self.log)) {
                Ok(store) => Some(store),
                Err(Error::Missing(_)) => None,
                Err(e) => return Err(e),
            },
        };
        st.store = reopened;
        Ok(())
    }

    /// Encode `records` for the store and queue them; None when there is nothing to write.
    fn submit(
        &self,
        st: &State,
        records: &[Record],
        sync: bool,
        member_size: Option<u64>,
    ) -> Result<Option<Queued>, Error> {
        let mut payload = String::new();
        for r in records {
            encode_record(r, self.format, &mut payload)?;
        }
        if payload.is_empty() {
            return Ok(None);
        }
        let markers = st.store.as_ref().map(Store::markers);
        let disk = markers.is_some_and(|m| m.write_ack == "disk");
        let member_size = member_size
            .or(markers.map(|m| m.member_size))
            .unwrap_or(crate::replay::DEFAULT_MEMBER_SIZE);
        let durable = sync || disk;
        let seq = self
            .batcher
            .submit(payload.into_bytes(), durable, member_size);
        Ok(Some(Queued { seq, durable }))
    }

    /// Answer one request (§21.7). Every failure is a status: the handler keeps serving
    /// (§21.10.3).
    pub fn handle(&self, req: &Request, bulk: Option<&[String]>) -> Response {
        let mut response = self.dispatch(req, bulk);
        if let Some(why) = self.batcher.failing() {
            response.diagnostics.push(format!(
                "{}: writes are failing ({why}); writes acknowledged before they were written may be lost",
                self.path.display()
            ));
        }
        response
    }

    fn dispatch(&self, req: &Request, bulk: Option<&[String]>) -> Response {
        match req.op.as_str() {
            "read" | "get" | "has" | "len" | "keys" => self.read(req),
            "set" | "append" | "delete" => self.write(req, bulk),
            "pop" | "popitem" | "setdefault" => self.change(req),
            "clear" | "scrub" => self.maintain(req.op == "clear"),
            "verify" => self.verify(),
            "parts" => self.parts(),
            other => Response::failed(1, format!("{other} is not answered here")),
        }
    }

    /// Build a response from a result and the warnings raised for it.
    fn answer(result: Result<Response, Error>, said: Vec<String>) -> Response {
        let mut response = result.unwrap_or_else(|e| Response::failed(1, e.to_string()));
        response.diagnostics = said;
        response
    }

    fn read(&self, req: &Request) -> Response {
        let (result, said) = self.locked(|st| {
            self.fresh(st)?;
            let Some(store) = &st.store else {
                return Ok(Response::failed(1, NO_STORE));
            };
            let mut out = Lines::default();
            let status = match req.op.as_str() {
                "read" => ops::read(store, &mut out)?,
                "get" => ops::get(store, &req.args, &mut out)?,
                "has" => ops::has(store, &req.args[0]),
                "len" => ops::len(store, &mut out)?,
                _ => ops::keys(store, &mut out)?,
            };
            let message = if store.incomplete() { INCOMPLETE } else { "" };
            Ok(Response {
                status,
                lines: out.0,
                diagnostics: Vec::new(),
                message: message.into(),
            })
        });
        Engine::answer(result, said)
    }

    fn write(&self, req: &Request, bulk: Option<&[String]>) -> Response {
        let delete = req.op == "delete";
        let records: Vec<Record> = if req.bulk() {
            let mut input = String::new();
            for line in bulk.unwrap_or_default() {
                input.push_str(line);
                input.push('\n');
            }
            let tsvz = Format {
                delimiter: b'\t',
                strict: true,
            };
            ops::parse_records(input.as_bytes(), tsvz, delete, Path::new("request"), None)
        } else if delete {
            req.raw
                .iter()
                .zip(&req.args)
                .map(|(raw, key)| record(raw, key, &[]))
                .collect()
        } else {
            vec![record(&req.raw[0], &req.args[0], &req.args[1..])]
        };
        let (result, said) = self.locked(|st| {
            let d = self.format.delimiter;
            if delete && ops::delete_needs_store(&records, d) {
                // Naming a key a reader would strip needs the store as it is now (§20.2.7),
                // read under the lock so that the delete is atomic (§21.8.2).
                self.fresh(st)?;
                let records = match &st.store {
                    Some(store) if store.incomplete() => {
                        return Err(Error::Refused(format!("{INCOMPLETE}; nothing was written")));
                    }
                    Some(store) => {
                        let strip = store.markers().strip;
                        ops::delete_records(&records, d, strip, |k| store.get(k).is_some())
                    }
                    None => ops::delete_records(&records, d, true, |_| false),
                };
                return self.submit(st, &records, req.sync, None);
            }
            self.submit(st, &records, req.sync, None)
        });
        let result = result.and_then(|queued| match queued {
            Some(q) if q.durable || req.written => {
                self.batcher.wait(q.seq, q.durable).map_err(Error::Refused)
            }
            _ => Ok(()),
        });
        Engine::answer(result.map(|()| Response::status(0)), said)
    }

    fn change(&self, req: &Request) -> Response {
        let (result, said) = self.locked(|st| {
            self.fresh(st)?;
            let plan = match (req.op.as_str(), &st.store) {
                ("setdefault", store) => {
                    ops::setdefault(store.as_ref(), self.format, &req.args[0], &req.args[1..])
                }
                (_, None) => return Ok((Response::failed(1, NO_STORE), None)),
                ("pop", Some(store)) => ops::pop(store, &req.args[0]),
                (_, Some(store)) => {
                    ops::popitem(store, req.args.first().map(String::as_str) == Some("first"))
                }
            };
            Ok(match plan {
                Plan::Exit(1) => (Response::failed(1, INCOMPLETE), None),
                Plan::Exit(code) => (Response::status(code), None),
                Plan::Show { key, cells } => (Engine::row(&key, &cells), None),
                Plan::Take {
                    key,
                    cells,
                    records,
                    member_size,
                } => {
                    let queued = self.submit(st, &records, req.sync, Some(member_size))?;
                    (Engine::row(&key, &cells), queued)
                }
                Plan::Put {
                    key,
                    cells,
                    record,
                    member_size,
                } => {
                    let queued = self.submit(st, &[record], req.sync, Some(member_size))?;
                    (Engine::row(&key, &cells), queued)
                }
            })
        });
        // The row is answered only once its write is made (TSVZ.py's handler).
        let result = result.and_then(|(response, queued)| match queued {
            Some(q) => self
                .batcher
                .wait(q.seq, q.durable)
                .map(|()| response)
                .map_err(Error::Refused),
            None => Ok(response),
        });
        Engine::answer(result, said)
    }

    fn row(key: &str, cells: &[String]) -> Response {
        Response {
            lines: vec![row_line(key, cells)],
            ..Response::status(0)
        }
    }

    fn maintain(&self, clear: bool) -> Response {
        let (result, said) = self.locked(|st| {
            self.batcher.barrier();
            let options = Engine::options(&st.said, &self.log);
            let outcome = if clear {
                maint::clear(&self.path, &options)
            } else {
                maint::scrub(&self.path, &options)
            };
            st.store = None;
            self.fresh(st)?;
            Ok(match outcome {
                Ok(Outcome::Written | Outcome::Unchanged) => Response::status(0),
                Ok(Outcome::Partial { backup }) => {
                    let kept = backup
                        .map(|b| format!("; its original bytes are in {}", b.display()))
                        .unwrap_or_default();
                    Response::failed(
                        1,
                        format!("the read stopped at damaged data; rewrote what was read{kept}"),
                    )
                }
                Err(e) => Response::failed(1, e.to_string()),
            })
        });
        Engine::answer(result, said)
    }

    fn verify(&self) -> Response {
        let (result, said) = self.locked(|st| {
            self.batcher.barrier();
            let options = Options {
                scope: Scope::KeysOnly,
                verify: true,
                ..Engine::options(&st.said, &self.log)
            };
            let store = match Store::open(&self.path, options) {
                Ok(store) => store,
                Err(Error::Missing(_)) => return Ok(Response::failed(1, NO_STORE)),
                Err(e) => return Err(e),
            };
            let mut out = Lines::default();
            let status = ops::verify(&store, &mut out)?;
            let message = if store.incomplete() { INCOMPLETE } else { "" };
            Ok(Response {
                status,
                lines: out.0,
                diagnostics: Vec::new(),
                message: message.into(),
            })
        });
        Engine::answer(result, said)
    }

    fn parts(&self) -> Response {
        let (result, said) = self.locked(|_| {
            self.batcher.barrier();
            let mut out = Lines::default();
            match ops::parts(&self.path, &mut out) {
                Ok(status) => Ok(Response {
                    lines: out.0,
                    ..Response::status(status)
                }),
                Err(Error::Missing(_)) => Ok(Response::failed(1, NO_STORE)),
                Err(e) => Err(e),
            }
        });
        Engine::answer(result, said)
    }

    /// Make every acknowledged write durable (`stop`, §21.9.3).
    pub fn flush(&self) -> Result<(), Error> {
        self.batcher.flush().map_err(Error::Refused)
    }

    /// Answer no more requests; write and fsync everything queued, and end the writer thread.
    pub fn close(&self) {
        lock(&self.state).closed = true;
        lock(&self.batcher.queue).closing = true;
        self.batcher.changed.notify_all();
        if let Some(thread) = lock(&self.thread).take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.close();
    }
}
```

- [ ] **Step 4: Run the tests to see them pass, several times**

Run: `for i in $(seq 1 20); do cargo test -q --all-features --lib handler::engine 2>&1 | grep -E 'test result' | grep -v ' 0 failed'; done; echo loops-done`
Expected: only `loops-done`: the 14 engine tests pass on every run (they use threads; a flaky failure is a bug).

Run: `cargo test --all-features 2>&1 | grep -E '^test result|FAILED|panicked'; cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -1`
Expected: 182 passed, 0 failed; no clippy warning.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "handler::engine: one store's live state, following the files, and its batching writer thread (§21.7-§21.9).

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_0198xw4TRnucPbiyi9csx4uj"
```

---

### Task 6: Pointer files (`handler::pointer`)

A handler takes over `STORE.serve` with `flock` (a stale file is unlinked, never written into, and created again exclusively without following links), publishes its address, host and pid with mode 0644, and removes the file when it stops (§21.2.3). A client trusts a pointer only as §21.2.4 says, connects only to a socket owned by the pointer's owner on this host, and checks the peer's uid after connecting.

**Files:**
- Create: `src/handler/pointer.rs`
- Modify: `src/handler/mod.rs`
- Test: `src/handler/pointer.rs` (unit tests; the other-owner checks run as root only)

**Interfaces:**
- Consumes: `protocol::{field, decode}`, `parts::list`.
- Produces (`crate::handler::pointer`, `#[doc(hidden)]`): `const MAGIC: &str = "tsvz-handler\t1"`; `fn path_of(store: &Path) -> PathBuf`; `fn hostname() -> String`; `struct Info { address: String, host: String, pid: String, owner: u32 }`; `fn lookup(store: &Path) -> Result<Option<Info>, String>` (Ok(None): no usable pointer and nothing to report; Err: why one is not trusted); `struct Unreachable { quiet: bool, message: String }`; `fn connect(info: &Info, timeout: Option<Duration>) -> Result<UnixStream, Unreachable>`; `fn peer_uid(stream: &UnixStream) -> io::Result<u32>`; `enum Take { Busy(HashMap<String, String>), Other(String) }`; `fn take(store: &Path) -> Result<Held, Take>`; `struct Held` with `fn publish(&mut self, address: &str) -> io::Result<()>` and `fn release(self)`.

- [ ] **Step 1: Write the failing tests**

Apply:

```diff
--- a/src/handler/mod.rs
+++ b/src/handler/mod.rs
@@ -4,6 +4,8 @@
 #[doc(hidden)]
 pub mod engine;
 #[doc(hidden)]
+pub mod pointer;
+#[doc(hidden)]
 pub mod protocol;
 
 pub use protocol::Response;
```

Create `src/handler/pointer.rs` holding only its test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::net::UnixListener;

    fn root() -> bool {
        // SAFETY: geteuid only reads the process's user id.
        unsafe { libc::geteuid() == 0 }
    }

    fn store(dir: &Path) -> PathBuf {
        let p = dir.join("s.tsvz");
        fs::write(&p, "k\tv\n").unwrap();
        p
    }

    fn plant(store: &Path, text: &str, mode: u32) {
        let p = path_of(store);
        fs::write(&p, text).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(mode)).unwrap();
    }

    fn text(address: &str, host: &str) -> String {
        format!("tsvz-handler\t1\naddress\t{address}\nhost\t{host}\npid\t1\n")
    }

    #[test]
    fn the_pointer_is_the_store_path_as_given_plus_serve() {
        assert_eq!(
            path_of(Path::new("d/s.tsvz.gz")),
            Path::new("d/s.tsvz.gz.serve")
        );
        assert!(!hostname().is_empty());
    }

    #[test]
    fn one_handler_holds_the_pointer_and_a_clean_release_removes_it() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let mut held = take(&s).unwrap();
        held.publish("unix:/nowhere/s").unwrap();
        let meta = fs::metadata(path_of(&s)).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o644);
        match take(&s) {
            Err(Take::Busy(fields)) => {
                assert_eq!(fields.get("pid"), Some(&std::process::id().to_string()))
            }
            other => panic!("second take: {:?}", other.err()),
        }
        let content = fs::read_to_string(path_of(&s)).unwrap();
        assert_eq!(
            content,
            format!(
                "tsvz-handler\t1\naddress\tunix:/nowhere/s\nhost\t{}\npid\t{}\n",
                field(&hostname(), false),
                std::process::id()
            )
        );
        held.release();
        assert!(!path_of(&s).exists());
        take(&s).unwrap().release();
    }

    #[test]
    fn a_stale_pointer_is_replaced_never_written_into() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        plant(&s, &text("unix:/gone", &hostname()), 0o600);
        let before = fs::metadata(path_of(&s)).unwrap().ino();
        // A hard link sees whether the old file was written into.
        let link = dir.path().join("old.serve");
        fs::hard_link(path_of(&s), &link).unwrap();
        let mut held = take(&s).unwrap();
        held.publish("unix:/new").unwrap();
        assert_ne!(fs::metadata(path_of(&s)).unwrap().ino(), before);
        assert_eq!(
            fs::read_to_string(&link).unwrap(),
            text("unix:/gone", &hostname())
        );
        held.release();
    }

    #[test]
    fn a_pointer_path_that_is_a_link_or_a_directory_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let victim = dir.path().join("victim.txt");
        fs::write(&victim, "do not touch\n").unwrap();
        std::os::unix::fs::symlink(&victim, path_of(&s)).unwrap();
        match take(&s) {
            Err(Take::Other(why)) => assert!(why.contains("not a regular file"), "{why}"),
            other => panic!("symlink: {:?}", other.err()),
        }
        assert_eq!(fs::read_to_string(&victim).unwrap(), "do not touch\n");
        assert!(lookup(&s).unwrap_err().contains("not a regular file"));
        fs::remove_file(path_of(&s)).unwrap();
        fs::create_dir(path_of(&s)).unwrap();
        assert!(matches!(take(&s), Err(Take::Other(_))));
    }

    #[test]
    fn clients_trust_only_a_safe_pointer() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let host = hostname();
        assert_eq!(lookup(&s), Ok(None), "no pointer file");
        plant(&s, &text("unix:/x/s", &host), 0o644);
        let info = lookup(&s).unwrap().unwrap();
        assert_eq!(
            (info.address.as_str(), info.host.as_str()),
            ("unix:/x/s", host.as_str())
        );
        assert_eq!(info.pid, "1");
        plant(&s, &text("unix:/x/s", &host), 0o620);
        assert!(lookup(&s).unwrap_err().contains("writable by others"));
        plant(
            &s,
            &text("unix:/x/s", &host).replace("handler\t1", "handler\t2"),
            0o600,
        );
        assert_eq!(lookup(&s), Ok(None), "not a pointer file of this protocol");
        plant(
            &s,
            &(text("unix:/x/s", &host) + "x-note\tbad\x07value\n"),
            0o600,
        );
        assert!(lookup(&s).unwrap_err().contains("control character"));
        plant(&s, &text("tcp:127.0.0.1:9", &host), 0o600);
        assert!(lookup(&s).unwrap_err().contains("not a Unix-domain socket"));
        plant(&s, "tsvz-handler\t1\npid\t1\n", 0o600);
        assert_eq!(lookup(&s), Ok(None), "an incomplete pointer is no handler");
        if root() {
            plant(&s, &text("unix:/x/s", &host), 0o600);
            std::os::unix::fs::chown(path_of(&s), Some(65534), Some(65534)).unwrap();
            assert!(lookup(&s).unwrap_err().contains("uid 65534"));
        }
    }

    #[test]
    fn connections_go_only_to_a_socket_of_the_pointers_owner_on_this_host() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("h.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        // SAFETY: geteuid only reads the process's user id.
        let me = unsafe { libc::geteuid() };
        let info = |address: String, host: String| Info {
            address,
            host,
            pid: "1".into(),
            owner: me,
        };
        let address = format!("unix:{}", sock.display());
        let stream = connect(&info(address.clone(), hostname()), None).unwrap();
        assert_eq!(peer_uid(&stream).unwrap(), me);
        let elsewhere = connect(&info(address.clone(), "other-host.invalid".into()), None);
        assert!(elsewhere.unwrap_err().quiet);
        let file = dir.path().join("plain");
        fs::write(&file, "").unwrap();
        let not_a_socket = connect(&info(format!("unix:{}", file.display()), hostname()), None);
        assert!(not_a_socket.unwrap_err().message.contains("not a socket"));
        if root() {
            listener.set_nonblocking(true).unwrap();
            while listener.accept().is_ok() {}
            std::os::unix::fs::chown(&sock, Some(65534), Some(65534)).unwrap();
            let theirs = connect(&info(address, hostname()), None);
            assert!(theirs.unwrap_err().message.contains("uid 65534"));
            assert!(listener.accept().is_err(), "it must not even connect");
        }
    }
}
```

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test --all-features --lib handler::pointer 2>&1 | grep -E '^error' | sort | uniq -c | head -5`
Expected: compile errors: `cannot find function path_of`, `take`, `lookup`, `connect`, `cannot find type Info`.

- [ ] **Step 3: Implement pointer files**

Put this above the test module in `src/handler/pointer.rs`:

```rust
//! The pointer file of a store (§21.2): how a handler announces itself, and how a client decides
//! whether to use it (§21.2.4).

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::handler::protocol::{decode, field};
use crate::parts;

/// The first line of a pointer file: its format and protocol version (§21.2.2).
pub const MAGIC: &str = "tsvz-handler\t1";
/// A larger file is not a pointer file.
const SIZE_LIMIT: u64 = 64 << 10;

/// The pointer file of `store`: the store path as given, plus `.serve` (§21.2.1).
pub fn path_of(store: &Path) -> PathBuf {
    let mut path = store.as_os_str().to_owned();
    path.push(".serve");
    PathBuf::from(path)
}

/// This host's name, as pointer files name it (§21.2.2).
pub fn hostname() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: gethostname writes at most buf.len() bytes into buf.
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if rc != 0 {
        return String::new();
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

/// Take `flock(LOCK_EX|LOCK_NB)` on `file`: false when another open file holds it.
fn try_flock(file: &File) -> io::Result<bool> {
    // SAFETY: flock only operates on the descriptor, which `file` keeps open.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(true);
    }
    let e = io::Error::last_os_error();
    if e.kind() == io::ErrorKind::WouldBlock {
        Ok(false)
    } else {
        Err(e)
    }
}

/// Whether `path` still names the open `file` (no link is followed).
fn names(path: &Path, file: &File) -> bool {
    match (std::fs::symlink_metadata(path), file.metadata()) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

/// Open a pointer file for reading without following a link or blocking on a FIFO.
fn open_pointer(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
}

/// Read a pointer file: Ok(None) when there is none, its fields (None when it is not a pointer
/// file of protocol 1) and metadata, or why the file there cannot be one.
#[allow(clippy::type_complexity)]
fn read(
    path: &Path,
) -> Result<Option<(Option<HashMap<String, String>>, std::fs::Metadata)>, String> {
    let mut file = match open_pointer(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{} is not a regular file ({e})", path.display())),
    };
    let meta = file
        .metadata()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    let mut data = Vec::new();
    Read::by_ref(&mut file)
        .take(SIZE_LIMIT + 1)
        .read_to_end(&mut data)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if data.len() as u64 > SIZE_LIMIT {
        return Err(format!(
            "{} is too large to be a pointer file",
            path.display()
        ));
    }
    let text = String::from_utf8_lossy(&data);
    let mut lines = text.split('\n');
    if lines.next() != Some(MAGIC) {
        return Ok(Some((None, meta)));
    }
    let fields = lines
        .filter_map(|line| line.split_once('\t'))
        .map(|(name, value)| (name.to_string(), decode(value)))
        .collect();
    Ok(Some((Some(fields), meta)))
}

/// What a trusted pointer file says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Info {
    /// `unix:PATH`.
    pub address: String,
    pub host: String,
    pub pid: String,
    /// The uid owning the pointer file: the socket and the handler must run as it.
    pub owner: u32,
}

/// The uid owning `store` (its first part), if it has one.
fn store_owner(store: &Path) -> Option<u32> {
    let list = parts::list(store).ok()?;
    list.iter()
        .find_map(|p| std::fs::metadata(&p.path).ok().map(|m| m.uid()))
}

/// The pointer file of `store`, checked as §21.2.4 says: Ok(None) when there is no usable
/// pointer file and nothing to report (none, not of this protocol, or incomplete); Err with why
/// one that exists is not trusted (a client warns, §20.8.2).
pub fn lookup(store: &Path) -> Result<Option<Info>, String> {
    let path = path_of(store);
    let mut found = None;
    for attempt in 0..2 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(50)); // a handler may be writing it
        }
        match read(&path)? {
            None => return Ok(None),
            Some((Some(fields), meta))
                if fields.contains_key("address") && fields.contains_key("host") =>
            {
                found = Some((fields, meta));
                break;
            }
            Some(_) => continue,
        }
    }
    let Some((fields, meta)) = found else {
        return Ok(None);
    };
    // SAFETY: geteuid only reads the process's user id.
    let me = unsafe { libc::geteuid() };
    if meta.uid() != me && Some(meta.uid()) != store_owner(store) {
        return Err(format!(
            "{} is owned by uid {}, neither you nor the owner of the store",
            path.display(),
            meta.uid()
        ));
    }
    if meta.mode() & 0o022 != 0 {
        return Err(format!(
            "{} is writable by others than its owner",
            path.display()
        ));
    }
    if let Some(name) = fields
        .iter()
        .find(|(_, value)| value.chars().any(|c| (c as u32) < 32))
        .map(|(name, _)| name)
    {
        return Err(format!(
            "{} has a control character in its {name}",
            path.display()
        ));
    }
    let address = fields["address"].clone();
    if !address.starts_with("unix:") {
        return Err(format!(
            "{} names {address}, which is not a Unix-domain socket",
            path.display()
        ));
    }
    Ok(Some(Info {
        address,
        host: fields["host"].clone(),
        pid: fields.get("pid").cloned().unwrap_or_default(),
        owner: meta.uid(),
    }))
}

/// Why a trusted pointer's handler cannot be used; `quiet` when that is expected (another host,
/// no permission to connect), so a client need not warn.
#[derive(Debug)]
pub struct Unreachable {
    pub quiet: bool,
    pub message: String,
}

fn unreachable(message: String) -> Unreachable {
    Unreachable {
        quiet: false,
        message,
    }
}

/// The uid of the process on the other end of `stream`.
pub fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    #[cfg(target_os = "linux")]
    {
        let mut cred = libc::ucred {
            pid: 0,
            uid: 0,
            gid: 0,
        };
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        // SAFETY: getsockopt writes at most `len` bytes into `cred`.
        let rc = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut cred as *mut libc::ucred).cast(),
                &mut len,
            )
        };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(cred.uid)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let (mut uid, mut gid) = (0, 0);
        // SAFETY: getpeereid writes the two ids.
        if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(uid)
    }
}

/// Connect to the handler a trusted pointer names (§21.2.4): on this host, through a socket
/// owned by the pointer's owner, to a process running as that user. `timeout` limits each
/// answer.
pub fn connect(info: &Info, timeout: Option<Duration>) -> Result<UnixStream, Unreachable> {
    if info.host != hostname() {
        return Err(Unreachable {
            quiet: true,
            message: format!("it runs on host {}", info.host),
        });
    }
    let path = Path::new(&info.address["unix:".len()..]);
    let meta = std::fs::symlink_metadata(path)
        .map_err(|e| unreachable(format!("{}: {e}", path.display())))?;
    if !meta.file_type().is_socket() {
        return Err(unreachable(format!("{} is not a socket", path.display())));
    }
    if meta.uid() != info.owner {
        return Err(unreachable(format!(
            "the socket {} is owned by uid {}, not by the owner of the pointer file",
            path.display(),
            meta.uid()
        )));
    }
    let deadline = Instant::now() + Duration::from_millis(500);
    let stream = loop {
        match UnixStream::connect(path) {
            Ok(stream) => break stream,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10)); // the listen queue is full
            }
            Err(e) => {
                let quiet = e.kind() == io::ErrorKind::PermissionDenied;
                return Err(Unreachable {
                    quiet,
                    message: format!("{}: {e}", path.display()),
                });
            }
        }
    };
    let uid = peer_uid(&stream).map_err(|e| unreachable(format!("{}: {e}", path.display())))?;
    if uid != info.owner {
        return Err(unreachable(format!(
            "the process behind {} runs as uid {uid}, not as the owner of the pointer file",
            path.display()
        )));
    }
    let _ = stream.set_read_timeout(timeout);
    Ok(stream)
}

/// Why a handler could not take the pointer file.
#[derive(Debug)]
pub enum Take {
    /// Another handler holds it; what its pointer file says (pid, host, …).
    Busy(HashMap<String, String>),
    /// Anything else: a path that is not a regular file, a file system without locks.
    Other(String),
}

fn busy(path: &Path) -> Take {
    Take::Busy(
        read(path)
            .ok()
            .flatten()
            .and_then(|r| r.0)
            .unwrap_or_default(),
    )
}

/// The locked pointer file of a running handler (§21.2.3).
pub struct Held {
    file: File,
    path: PathBuf,
}

/// Take over the pointer file of `store` (§21.2.3): busy when another handler holds its lock;
/// a stale one is removed, never written into, and a new one created exclusively without
/// following a link, mode 0600 until it is published.
pub fn take(store: &Path) -> Result<Held, Take> {
    let path = path_of(store);
    let other = |e: io::Error| Take::Other(format!("{}: {e}", path.display()));
    let old = match open_pointer(&path) {
        Ok(file) => Some(file),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => {
            return Err(Take::Other(format!(
                "{} is not a regular file ({e}); remove it",
                path.display()
            )));
        }
    };
    if let Some(old) = &old {
        if !old.metadata().map_err(other)?.is_file() {
            return Err(Take::Other(format!(
                "{} is not a regular file; remove it",
                path.display()
            )));
        }
        if !try_flock(old).map_err(other)? {
            return Err(busy(&path));
        }
        if names(&path, old) {
            std::fs::remove_file(&path).map_err(other)?;
        }
    }
    let file = match OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Err(busy(&path)),
        Err(e) => return Err(other(e)),
    };
    drop(old);
    match try_flock(&file) {
        Ok(true) if names(&path, &file) => Ok(Held { file, path }),
        Ok(_) => Err(busy(&path)),
        Err(e) => {
            let _ = std::fs::remove_file(&path);
            Err(other(e))
        }
    }
}

impl Held {
    /// Write the pointer for a handler at `address` (`unix:PATH`): readable by all, writable
    /// by its owner only (§21.2.3), and synced.
    pub fn publish(&mut self, address: &str) -> io::Result<()> {
        let text = format!(
            "{MAGIC}\naddress\t{}\nhost\t{}\npid\t{}\n",
            field(address, false),
            field(&hostname(), false),
            std::process::id()
        );
        self.file
            .set_permissions(std::fs::Permissions::from_mode(0o644))?;
        self.file.set_len(0)?;
        self.file.rewind()?;
        self.file.write_all(text.as_bytes())?;
        self.file.sync_all()
    }

    /// Remove the pointer file if its path still names this handler's file, and unlock it.
    pub fn release(self) {
        if names(&self.path, &self.file) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}
```

- [ ] **Step 4: Run the tests to see them pass**

Run: `cargo test --all-features 2>&1 | grep -E '^test result|FAILED|panicked'; cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -1`
Expected: the 6 pointer tests pass; 188 passed in total, 0 failed; no clippy warning.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "handler::pointer: take over, publish and release a pointer file; trust and connect as §21.2.4 says.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_0198xw4TRnucPbiyi9csx4uj"
```

---
### Task 7: `serve` and `stop` (`handler::server`)

`serve STORE` takes the pointer file, blocks SIGINT, SIGTERM and SIGHUP so one thread receives them with `sigwait` (one ignored at start, as under `nohup`, stays ignored), opens the engine (creating a missing store), binds the socket in a new `tsvz-XXXXXX` directory, publishes the pointer, and accepts connections, one thread each. A `stop` request (after a flush), a signal or `--x-idle-timeout` stops it: the accept loop is woken by a connection of the handler's own, open connections are shut down, the engine drains and fsyncs, and the socket directory and pointer file are removed (§21.13.1). `stop STORE` asks a handler to stop and waits until its pointer is gone (§21.13.2). The CLI gains `--x-direct` (used by Task 9), `--x-idle-timeout S`, `--x-group G` and `--x-mode M`, spelled as in TSVZ.py. The old CLI test that expected `serve` to exit 1 goes: it would now serve forever.

**Files:**
- Create: `src/handler/server.rs`, `src/cli/serve.rs`, `tests/handler.rs`
- Modify: `src/handler/mod.rs`, `src/cli/args.rs`, `src/cli/mod.rs`, `tests/cli.rs`
- Test: `tests/handler.rs` (a real `tsvz serve`, through its socket)

**Interfaces:**
- Consumes: `Engine` (Task 5), `pointer::{take, Take, Held, lookup, connect, path_of}` (Task 6), `protocol::{parse, read_line, read_response, Response, LINE_LIMIT}` (Task 4).
- Produces: `handler::server::{Config, InfoFn, serve, stop}`: `pub type InfoFn = Arc<dyn Fn(&str) + Send + Sync>`; `pub struct Config { idle: Option<Duration>, group: Option<u32>, mode: Option<u32>, log: Option<WarnFn>, info: InfoFn, verbose: bool }`; `pub fn serve(store: &Path, config: Config) -> Result<(), Error>` (returns after a clean stop); `pub fn stop(store: &Path) -> Result<(), Error>`. `cli::args::Args` gains `direct: bool`, `idle_timeout: Option<Duration>`, `group: Option<u32>`, `mode: Option<u32>`. With `-v`, `serve` reports `PATH: OP answered #N` for every request.

- [ ] **Step 1: Write the failing tests**

Create `tests/handler.rs`:

```rust
//! A real `tsvz serve`, driven over its socket and through the command line (§20.8, §21).

use std::fs;
use std::io::{BufReader, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use tsvz::handler::Response;
use tsvz::handler::pointer;
use tsvz::handler::protocol::read_response;

fn tsvz() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tsvz"))
}

/// A running `tsvz serve`; killed if a test leaves it running.
struct Served {
    child: Child,
    stderr: PathBuf,
}

impl Served {
    fn start(store: &Path, extra: &[&str]) -> Served {
        let stderr = store.with_extension("serve-stderr");
        let child = tsvz()
            .arg("serve")
            .arg(store)
            .args(extra)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(fs::File::create(&stderr).unwrap())
            .spawn()
            .unwrap();
        let mut served = Served { child, stderr };
        let deadline = Instant::now() + Duration::from_secs(10);
        while connect(store).is_none() {
            if let Some(status) = served.child.try_wait().unwrap() {
                panic!("serve exited {status}: {}", served.stderr());
            }
            assert!(Instant::now() < deadline, "no handler: {}", served.stderr());
            std::thread::sleep(Duration::from_millis(20));
        }
        served
    }

    fn stderr(&self) -> String {
        fs::read_to_string(&self.stderr).unwrap_or_default()
    }

    /// Wait for the handler to exit; its exit status.
    fn wait(&mut self) -> i32 {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status.code().unwrap_or(-1);
            }
            assert!(
                Instant::now() < deadline,
                "serve did not exit: {}",
                self.stderr()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn signal(&self, signal: i32) {
        // SAFETY: kill only sends a signal to the child, which is still ours to reap.
        unsafe { libc::kill(self.child.id() as i32, signal) };
    }
}

impl Drop for Served {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A connection to the store's handler, through a trusted pointer file.
fn connect(store: &Path) -> Option<UnixStream> {
    let info = pointer::lookup(store).ok()??;
    pointer::connect(&info, Some(Duration::from_secs(10))).ok()
}

/// Send raw request bytes on `conn` and read one response.
fn send(conn: &mut UnixStream, bytes: &[u8]) -> Response {
    conn.write_all(bytes).unwrap();
    read_response(&mut BufReader::new(conn.try_clone().unwrap())).unwrap()
}

fn ask(store: &Path, line: &str) -> Response {
    let mut conn = connect(store).expect("a handler");
    send(&mut conn, format!("{line}\n").as_bytes())
}

fn run(dir: &Path, args: &[&str], stdin: &[u8]) -> Output {
    let mut child = tsvz()
        .current_dir(dir)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    child.wait_with_output().unwrap()
}

fn text(out: &[u8]) -> String {
    String::from_utf8_lossy(out).into_owned()
}

fn store(dir: &Path, content: &str) -> PathBuf {
    let p = dir.join("s.tsvz");
    fs::write(&p, content).unwrap();
    p
}

#[test]
fn a_client_that_hangs_up_before_its_answer_leaves_the_handler_serving() {
    let dir = tempfile::tempdir().unwrap();
    let rows: String = (0..50_000)
        .map(|i| format!("k{i}\t{}\n", "v".repeat(40)))
        .collect();
    let s = store(dir.path(), &rows);
    let _served = Served::start(&s, &[]);
    for _ in 0..5 {
        let mut conn = connect(&s).unwrap();
        conn.write_all(b"read\n").unwrap();
    }
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(ask(&s, "len").lines, ["50000"]);
}

#[test]
fn stop_ends_the_connections_still_open() {
    use std::io::Read;
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path(), "k\tv\n");
    let mut served = Served::start(&s, &[]);
    let mut idle = connect(&s).unwrap();
    idle.write_all(b"len").unwrap(); // a request never finished
    let stop = run(dir.path(), &["stop", "s.tsvz"], b"");
    assert_eq!(stop.status.code(), Some(0), "{}", text(&stop.stderr));
    assert_eq!(served.wait(), 0);
    idle.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut byte = [0u8; 1];
    assert_eq!(idle.read(&mut byte).unwrap_or(0), 0, "closed");
}

#[test]
fn a_socket_path_too_long_to_bind_fails_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path(), "k\tv\n");
    let deep = dir.path().join("d".repeat(120));
    fs::create_dir(&deep).unwrap();
    // --x-mode puts the socket under the temporary directory, here one too deep to bind in.
    let out = tsvz()
        .current_dir(dir.path())
        .env("TMPDIR", &deep)
        .args(["serve", "s.tsvz", "--x-mode", "600"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert!(!text(&out.stderr).is_empty());
    assert!(
        !pointer::path_of(&s).exists(),
        "no pointer file is left behind"
    );
    assert_eq!(
        fs::read_dir(&deep).unwrap().count(),
        0,
        "nor a socket directory"
    );
}

#[test]
fn serve_answers_until_stop_and_leaves_nothing_behind() {
    let dir = tempfile::tempdir().unwrap();
    let s = dir.path().join("s.tsvz");
    let mut served = Served::start(&s, &[]);
    assert_eq!(fs::read(&s).unwrap(), b"", "serve creates a missing store");
    assert_eq!(ask(&s, "set\tk\tv").status, 0);
    assert_eq!(ask(&s, "get\tk").lines, ["k\tv"]);
    let info = pointer::lookup(&s).unwrap().unwrap();
    let socket = PathBuf::from(&info.address["unix:".len()..]);
    let stop = run(dir.path(), &["stop", "s.tsvz"], b"");
    assert_eq!(stop.status.code(), Some(0), "{}", text(&stop.stderr));
    assert_eq!(served.wait(), 0, "{}", served.stderr());
    assert!(
        !pointer::path_of(&s).exists(),
        "the pointer file is removed"
    );
    assert!(
        !socket.parent().unwrap().exists(),
        "the socket directory is removed"
    );
    assert_eq!(fs::read_to_string(&s).unwrap(), "k\tv\n");
    assert!(served.stderr().contains("serving"), "{}", served.stderr());
}

#[test]
fn a_second_serve_exits_1_and_the_first_keeps_serving() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path(), "k\tv\n");
    let _served = Served::start(&s, &[]);
    let second = run(dir.path(), &["serve", "s.tsvz"], b"");
    assert_eq!(second.status.code(), Some(1));
    assert!(
        text(&second.stderr).contains("already served by pid"),
        "{}",
        text(&second.stderr)
    );
    assert_eq!(ask(&s, "len").lines, ["1"]);
}

#[test]
fn signals_stop_the_handler_cleanly() {
    for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), "");
        let mut served = Served::start(&s, &[]);
        let mut conn = connect(&s).unwrap();
        for i in 0..50 {
            assert_eq!(
                send(&mut conn, format!("set\tk{i}\tv\n").as_bytes()).status,
                0
            );
        }
        served.signal(signal);
        assert_eq!(served.wait(), 0, "signal {signal}: {}", served.stderr());
        assert!(!pointer::path_of(&s).exists(), "signal {signal}");
        assert_eq!(
            fs::read_to_string(&s).unwrap().lines().count(),
            50,
            "signal {signal}"
        );
    }
}

#[test]
fn an_idle_handler_stops_after_its_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path(), "");
    let mut served = Served::start(&s, &["--x-idle-timeout", "0.5"]);
    assert_eq!(served.wait(), 0);
    assert!(served.stderr().contains("idle"), "{}", served.stderr());
}

#[test]
fn the_socket_is_private_unless_widened() {
    let mode_of = |s: &Path| {
        let info = pointer::lookup(s).unwrap().unwrap();
        let socket = PathBuf::from(&info.address["unix:".len()..]);
        let dir = fs::metadata(socket.parent().unwrap()).unwrap();
        let sock = fs::metadata(&socket).unwrap();
        (sock.mode() & 0o777, dir.mode() & 0o777, sock.gid())
    };
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path(), "");
    {
        let _served = Served::start(&s, &[]);
        let (sock, d, _) = mode_of(&s);
        assert_eq!((sock, d), (0o600, 0o700));
    }
    {
        let _served = Served::start(&s, &["--x-mode", "666"]);
        let (sock, d, _) = mode_of(&s);
        assert_eq!((sock, d), (0o666, 0o711));
    }
    // SAFETY: getegid only reads the process's group id.
    let gid = unsafe { libc::getegid() };
    {
        let _served = Served::start(&s, &["--x-group", &gid.to_string()]);
        let (sock, d, g) = mode_of(&s);
        assert_eq!((sock, d, g), (0o660, 0o710, gid));
    }
    let bad = run(dir.path(), &["serve", "s.tsvz", "--x-mode", "999"], b"");
    assert_eq!(bad.status.code(), Some(2));
}

#[test]
fn serve_takes_a_strict_store_and_stop_needs_a_handler() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("s.tsv"), "k\tv\n").unwrap();
    let loose = run(dir.path(), &["serve", "s.tsv"], b"");
    assert_eq!(loose.status.code(), Some(1));
    assert!(
        text(&loose.stderr).contains("strict"),
        "{}",
        text(&loose.stderr)
    );
    assert!(!dir.path().join("s.tsv.serve").exists());
    store(dir.path(), "");
    let stop = run(dir.path(), &["stop", "s.tsvz"], b"");
    assert_eq!(stop.status.code(), Some(1));
    assert!(!text(&stop.stderr).is_empty());
}

#[test]
fn a_bulk_request_cut_off_before_its_end_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path(), "");
    let _served = Served::start(&s, &[]);
    let mut conn = connect(&s).unwrap();
    conn.write_all(b"set\t-\na\t1\nb\t2\n").unwrap();
    drop(conn);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(ask(&s, "len").lines, ["0"]);
    assert_eq!(fs::read(&s).unwrap(), b"");
}

#[test]
fn one_connection_carries_many_requests_including_bad_ones() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path(), "a\tx\n");
    let _served = Served::start(&s, &[]);
    let mut conn = connect(&s).unwrap();
    assert_eq!(send(&mut conn, b"get\n").status, 2);
    assert_eq!(send(&mut conn, b"bogus\tk\n").status, 2);
    assert_eq!(send(&mut conn, b"len\r\n").lines, ["1"]);
    let big = "x".repeat((1 << 20) - 16);
    assert_eq!(
        send(&mut conn, format!("set\tbig\t{big}\n").as_bytes()).status,
        0
    );
    let r = send(&mut conn, b"get\tbig\n");
    assert_eq!(r.lines, [format!("big\t{big}")]);
    assert_eq!(send(&mut conn, b"set\t-\nb\t2\n#\n").status, 0);
    assert_eq!(send(&mut conn, b"keys\n").lines, ["a", "big", "b"]);
}

#[test]
fn a_stale_pointer_is_taken_over() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path(), "k\tv\n");
    let p = pointer::path_of(&s);
    fs::write(
        &p,
        format!(
            "tsvz-handler\t1\naddress\tunix:{}\nhost\t{}\npid\t999999999\n",
            dir.path().join("gone.sock").display(),
            pointer::hostname()
        ),
    )
    .unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o600)).unwrap();
    let _served = Served::start(&s, &[]);
    assert_eq!(ask(&s, "len").lines, ["1"]);
}
```

Apply (the obsolete test):

```diff
--- a/tests/cli.rs
+++ b/tests/cli.rs
@@ -229,13 +229,6 @@ fn a_stopped_read_prints_what_it_read_and_exits_1() {
     assert_eq!((code, out.as_str()), (1, "a\t1\n"));
 }
 
-#[test]
-fn serve_and_stop_are_not_provided() {
-    let d = store("a\tx\n");
-    assert_eq!(records(d.path(), &["serve", "s.tsvz"]).0, 1);
-    assert_eq!(records(d.path(), &["stop", "s.tsvz"]).0, 1);
-}
-
 fn file(dir: &Path, name: &str) -> String {
     String::from_utf8(fs::read(dir.join(name)).unwrap()).unwrap()
 }
```

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test --all-features --test handler 2>&1 | grep -E 'test result|panicked' | head -3`
Expected: `test result: FAILED. 0 passed; 12 failed`: `serve` still exits 1 ("this tsvz does not provide a handler"), and `--x-mode` is an unknown option.

- [ ] **Step 3: Implement the server**

Create `src/handler/server.rs`:

```rust
//! `serve` and `stop` (§21.13): the socket, its connections, and stopping cleanly.

use std::collections::HashMap;
use std::ffi::{CString, OsString};
use std::io::{BufReader, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::handler::engine::Engine;
use crate::handler::pointer::{self, Take};
use crate::handler::protocol::{self, LINE_LIMIT, Response, read_line, read_response};
use crate::{Error, WarnFn, name};

/// Where `serve` reports what it does (one line each): starting, stopping, and with `verbose`
/// every request.
pub type InfoFn = Arc<dyn Fn(&str) + Send + Sync>;

/// How to serve.
pub struct Config {
    /// Stop after this long with no open connection (`--x-idle-timeout`).
    pub idle: Option<Duration>,
    /// Let this group connect (`--x-group`): the socket becomes 0660 unless `mode` says else.
    pub group: Option<u32>,
    /// The socket's mode (`--x-mode`).
    pub mode: Option<u32>,
    /// Warnings and failed writes.
    pub log: Option<WarnFn>,
    pub info: InfoFn,
    /// Report every request and its status.
    pub verbose: bool,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn io(path: &Path) -> impl Fn(std::io::Error) -> Error + '_ {
    move |e| Error::io(path, e)
}

/// What the connections and the stoppers share.
struct Shared {
    stopping: AtomicBool,
    socket: PathBuf,
    /// Open connections (for the idle timeout) and when the last one closed.
    activity: Mutex<(usize, Instant)>,
    /// A handle on each open connection, to end it when the handler stops.
    open: Mutex<HashMap<u64, UnixStream>>,
    next: AtomicU64,
}

impl Shared {
    /// Make the accept loop return: set the flag, then wake it with a connection of our own.
    fn stop(&self) {
        if !self.stopping.swap(true, Ordering::SeqCst) {
            let _ = UnixStream::connect(&self.socket);
        }
    }
}

/// Block SIGINT, SIGTERM and SIGHUP in this thread and every thread it starts, so that one
/// thread receives them with `sigwait`; a signal ignored when `serve` starts (`nohup`) stays
/// ignored. The blocked set; empty when all three are ignored.
fn block_signals() -> libc::sigset_t {
    // SAFETY: the set is initialized by sigemptyset before use; sigaction with a null new
    // action only reads the current one; pthread_sigmask changes this thread's mask.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            let mut old: libc::sigaction = std::mem::zeroed();
            libc::sigaction(signal, std::ptr::null(), &mut old);
            if old.sa_sigaction != libc::SIG_IGN {
                libc::sigaddset(&mut set, signal);
            }
        }
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
        set
    }
}

fn wait_for_signal(set: libc::sigset_t, shared: Arc<Shared>, info: InfoFn) {
    let mut signal = 0;
    // SAFETY: sigwait reads the set and writes the signal number.
    if unsafe { libc::sigwait(&set, &mut signal) } == 0 {
        info(&format!("signal {signal}; stopping"));
        shared.stop();
    }
}

/// A new directory for the socket: under `$XDG_RUNTIME_DIR` when it is a writable directory and
/// the socket is private, else under the temporary directory (others cannot enter a private
/// runtime directory).
fn socket_dir(shared: bool) -> Result<PathBuf, Error> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|d| !shared && d.is_dir() && writable(d));
    let base = runtime.unwrap_or_else(std::env::temp_dir);
    let template = base.join("tsvz-XXXXXX");
    let mut bytes = template.as_os_str().as_bytes().to_vec();
    bytes.push(0);
    // SAFETY: mkdtemp rewrites the trailing XXXXXX of the NUL-terminated template in place.
    let made = unsafe { libc::mkdtemp(bytes.as_mut_ptr().cast()) };
    if made.is_null() {
        return Err(Error::io(&template, std::io::Error::last_os_error()));
    }
    bytes.pop();
    Ok(PathBuf::from(OsString::from_vec(bytes)))
}

fn writable(dir: &Path) -> bool {
    let Ok(c) = CString::new(dir.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: access only reads the NUL-terminated path.
    unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 }
}

/// Bind the handler's socket in `dir` with the access `config` asks for (§21.11).
fn bind(dir: &Path, config: &Config) -> Result<(UnixListener, PathBuf), Error> {
    let path = dir.join("s");
    let listener = UnixListener::bind(&path).map_err(io(&path))?;
    if let Some(gid) = config.group {
        std::os::unix::fs::chown(dir, None, Some(gid)).map_err(io(dir))?;
        std::os::unix::fs::chown(&path, None, Some(gid)).map_err(io(&path))?;
    }
    let mode = config
        .mode
        .unwrap_or(if config.group.is_some() { 0o660 } else { 0o600 });
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).map_err(io(&path))?;
    let search = 0o700
        | if mode & 0o070 != 0 { 0o010 } else { 0 }
        | if mode & 0o007 != 0 { 0o001 } else { 0 };
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(search)).map_err(io(dir))?;
    Ok((listener, path))
}

/// Run a handler for `store` in this thread until it is stopped (§21.13.1): by a `stop`
/// request, a signal, or the idle timeout. Errors: another handler serves the store (its pid
/// and host are in the message), the store is loose, or the files or socket cannot be set up.
pub fn serve(store: &Path, config: Config) -> Result<(), Error> {
    if !name::format_of(store).is_some_and(|f| f.strict) {
        return Err(Error::Refused(format!(
            "{}: serve takes a strict store (.tsvz .csvz .nsvz .psvz)",
            store.display()
        )));
    }
    let mut held = match pointer::take(store) {
        Ok(held) => held,
        Err(Take::Busy(fields)) => {
            let say = |name: &str| fields.get(name).cloned().unwrap_or_else(|| "?".into());
            return Err(Error::Refused(format!(
                "{}: already served by pid {} on host {}",
                store.display(),
                say("pid"),
                say("host")
            )));
        }
        Err(Take::Other(why)) => {
            return Err(Error::Refused(format!(
                "{}: cannot take the pointer file: {why}",
                store.display()
            )));
        }
    };
    let signals = block_signals();
    let engine = match Engine::open(store, config.log.clone()) {
        Ok(engine) => Arc::new(engine),
        Err(e) => {
            held.release();
            return Err(e);
        }
    };
    let shared_access = config.group.is_some() || config.mode.is_some();
    let started = socket_dir(shared_access).and_then(|dir| match bind(&dir, &config) {
        Ok((listener, path)) => Ok((dir, listener, path)),
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            Err(e)
        }
    });
    let (dir, listener, socket) = match started {
        Ok(started) => started,
        Err(e) => {
            engine.close();
            held.release();
            return Err(e);
        }
    };
    let address = format!("unix:{}", socket.display());
    if let Err(e) = held.publish(&address) {
        engine.close();
        let _ = std::fs::remove_dir_all(&dir);
        held.release();
        return Err(Error::io(&pointer::path_of(store), e));
    }
    let shared = Arc::new(Shared {
        stopping: AtomicBool::new(false),
        socket,
        activity: Mutex::new((0, Instant::now())),
        open: Mutex::new(HashMap::new()),
        next: AtomicU64::new(0),
    });
    // SAFETY: sigismember only reads the set.
    let any = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP]
        .iter()
        .any(|&s| unsafe { libc::sigismember(&signals, s) } == 1);
    if any {
        let (shared, info) = (shared.clone(), config.info.clone());
        std::thread::spawn(move || wait_for_signal(signals, shared, info));
    }
    if let Some(idle) = config.idle {
        let (shared, info) = (shared.clone(), config.info.clone());
        std::thread::spawn(move || watch_idle(idle, shared, info));
    }
    (config.info)(&format!(
        "serving {} at {address} (pid {})",
        store.display(),
        std::process::id()
    ));
    for conn in listener.incoming() {
        if shared.stopping.load(Ordering::SeqCst) {
            break;
        }
        let Ok(stream) = conn else {
            std::thread::sleep(Duration::from_millis(10)); // out of descriptors, for a moment
            continue;
        };
        let (engine, shared, info) = (engine.clone(), shared.clone(), config.info.clone());
        let verbose = config.verbose;
        std::thread::spawn(move || serve_connection(stream, &engine, &shared, &info, verbose));
    }
    drop(listener);
    for (_, conn) in lock(&shared.open).drain() {
        let _ = conn.shutdown(std::net::Shutdown::Both);
    }
    engine.close();
    let _ = std::fs::remove_dir_all(&dir);
    held.release();
    (config.info)(&format!("stopped serving {}", store.display()));
    Ok(())
}

fn watch_idle(idle: Duration, shared: Arc<Shared>, info: InfoFn) {
    let tick = (idle / 4).clamp(Duration::from_millis(10), Duration::from_secs(1));
    while !shared.stopping.load(Ordering::SeqCst) {
        std::thread::sleep(tick);
        let (open, since) = *lock(&shared.activity);
        if open == 0 && since.elapsed() >= idle {
            info(&format!("idle for {} s; stopping", idle.as_secs_f64()));
            shared.stop();
        }
    }
}

/// Answer the requests of one connection, one at a time (§21.3.2), until it ends.
fn serve_connection(
    stream: UnixStream,
    engine: &Engine,
    shared: &Shared,
    info: &InfoFn,
    verbose: bool,
) {
    let id = shared.next.fetch_add(1, Ordering::SeqCst);
    if let Ok(handle) = stream.try_clone() {
        lock(&shared.open).insert(id, handle);
    }
    lock(&shared.activity).0 += 1;
    if let Ok(reader) = stream.try_clone() {
        answer(
            BufReader::new(reader),
            stream,
            engine,
            shared,
            info,
            verbose,
        );
    }
    lock(&shared.open).remove(&id);
    let mut activity = lock(&shared.activity);
    activity.0 -= 1;
    activity.1 = Instant::now();
}

fn answer(
    mut reader: BufReader<UnixStream>,
    mut writer: UnixStream,
    engine: &Engine,
    shared: &Shared,
    info: &InfoFn,
    verbose: bool,
) {
    // A read error, a line over the limit or the end of the connection ends it (§21.10.2).
    while let Ok(Some(line)) = read_line(&mut reader, LINE_LIMIT) {
        let request = protocol::parse(&line);
        let mut bulk = None;
        if let Ok(r) = &request {
            if r.bulk() {
                let mut records = Vec::new();
                loop {
                    match read_line(&mut reader, LINE_LIMIT) {
                        Ok(Some(l)) if l == "#" => break,
                        Ok(Some(l)) => records.push(l),
                        _ => return, // cut off before its end: nothing is written (§21.7.2)
                    }
                }
                bulk = Some(records);
            }
        }
        let response = match &request {
            Ok(r) if r.op == "stop" => {
                let response = match engine.flush() {
                    Ok(()) => Response::status(0),
                    Err(e) => Response::failed(1, e.to_string()),
                };
                let _ = writer.write_all(&response.encode());
                shared.stop();
                return;
            }
            Ok(r) => engine.handle(r, bulk.as_deref()),
            Err(message) => Response::failed(2, message.clone()),
        };
        if verbose {
            let op = request.as_ref().map_or("?", |r| r.op.as_str());
            info(&format!(
                "{}: {op} answered #{}",
                engine.path().display(),
                response.status
            ));
        }
        if writer.write_all(&response.encode()).is_err() {
            return;
        }
    }
}

/// Ask the handler of `store` to stop and wait until it has (§21.13.2).
pub fn stop(store: &Path) -> Result<(), Error> {
    let refused = |why: String| Error::Refused(format!("{}: {why}", store.display()));
    let info = match pointer::lookup(store) {
        Ok(Some(info)) => info,
        Ok(None) => return Err(refused("no handler is running".into())),
        Err(why) => return Err(refused(why)),
    };
    let mut conn = pointer::connect(&info, None)
        .map_err(|e| refused(format!("the handler does not answer ({})", e.message)))?;
    conn.write_all(b"stop\n")
        .map_err(|e| refused(format!("the handler does not answer ({e})")))?;
    let mut reader = BufReader::new(&conn);
    let response =
        read_response(&mut reader).map_err(|e| refused(format!("the handler went away ({e})")))?;
    if response.status != 0 {
        return Err(refused(format!("stop failed: {}", response.message)));
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline
        && pointer::lookup(store)
            .ok()
            .flatten()
            .is_some_and(|now| now.pid == info.pid)
    {
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}
```

Apply:

```diff
--- a/src/handler/mod.rs
+++ b/src/handler/mod.rs
@@ -7,5 +7,7 @@ pub mod engine;
 pub mod pointer;
 #[doc(hidden)]
 pub mod protocol;
+#[doc(hidden)]
+pub mod server;
 
 pub use protocol::Response;
```

Create `src/cli/serve.rs`:

```rust
//! `serve` and `stop` from the command line (§21.13).

use std::sync::Arc;

use crate::Error;
use crate::cli::Ctx;
use crate::handler::server::{self, Config, InfoFn};

pub(crate) fn serve(ctx: &mut Ctx) -> Result<i32, Error> {
    let quiet = ctx.args.quiet;
    let info: InfoFn = Arc::new(move |message: &str| {
        if !quiet {
            diag!("tsvz: {message}");
        }
    });
    let config = Config {
        idle: ctx.args.idle_timeout,
        group: ctx.args.group,
        mode: ctx.args.mode,
        log: Some(ctx.warn.clone()),
        info,
        verbose: ctx.args.verbose,
    };
    server::serve(&ctx.store, config)?;
    Ok(0)
}

pub(crate) fn stop(ctx: &mut Ctx) -> Result<i32, Error> {
    server::stop(&ctx.store)?;
    Ok(0)
}
```

Apply:

```diff
--- a/src/cli/args.rs
+++ b/src/cli/args.rs
@@ -1,6 +1,7 @@
 //! §20.1 argument parsing.
 
 use std::collections::VecDeque;
+use std::time::Duration;
 
 /// The operations of §20.2, plus `serve` and `stop` (§20.2.6).
 pub const OPERATIONS: &[&str] = &[
@@ -36,6 +37,14 @@ pub struct Args {
     pub quiet: bool,
     pub verbose: bool,
     pub delimiter: Option<u8>,
+    /// `--x-direct`: work on the files even when a handler serves the store.
+    pub direct: bool,
+    /// `--x-idle-timeout S` (serve).
+    pub idle_timeout: Option<Duration>,
+    /// `--x-group G` (serve), as a gid.
+    pub group: Option<u32>,
+    /// `--x-mode M` (serve): the socket's mode.
+    pub mode: Option<u32>,
     pub op: String,
     pub store: String,
     pub args: Vec<String>,
@@ -61,6 +70,10 @@ const OPTIONS: &[(&str, &str, bool)] = &[
     ("--verbose", "verbose", false),
     ("-d", "delimiter", true),
     ("--delimiter", "delimiter", true),
+    ("--x-direct", "direct", false),
+    ("--x-idle-timeout", "idle", true),
+    ("--x-group", "group", true),
+    ("--x-mode", "mode", true),
 ];
 
 /// An option's destination and whether it takes a value; a long option may be given by a
@@ -91,6 +104,39 @@ fn is_negative_number(arg: &str) -> bool {
     chars.next() == Some('-') && chars.next().is_some_and(|c| c.is_ascii_digit() || c == '.')
 }
 
+fn parse_idle(text: &str) -> Result<Duration, String> {
+    text.parse::<f64>()
+        .ok()
+        .and_then(|s| Duration::try_from_secs_f64(s).ok())
+        .ok_or_else(|| format!("--x-idle-timeout takes seconds, not {text:?}"))
+}
+
+fn parse_mode(text: &str) -> Result<u32, String> {
+    u32::from_str_radix(text, 8)
+        .ok()
+        .filter(|&m| m <= 0o777)
+        .ok_or_else(|| format!("--x-mode takes an octal mode such as 660, not {text:?}"))
+}
+
+/// A group name or number as a gid.
+fn parse_group(text: &str) -> Result<u32, String> {
+    if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) {
+        return text
+            .parse()
+            .map_err(|_| format!("--x-group: bad group {text:?}"));
+    }
+    let name =
+        std::ffi::CString::new(text).map_err(|_| format!("--x-group: bad group {text:?}"))?;
+    // SAFETY: getgrnam reads the NUL-terminated name; the entry it returns is read at once,
+    // before any other call could reuse it.
+    let entry = unsafe { libc::getgrnam(name.as_ptr()) };
+    if entry.is_null() {
+        return Err(format!("--x-group: no group named {text:?}"));
+    }
+    // SAFETY: entry is a valid group entry (checked above).
+    Ok(unsafe { (*entry).gr_gid })
+}
+
 fn parse_delimiter(text: &str) -> Result<u8, String> {
     match text {
         "tab" => Ok(b'\t'),
@@ -155,6 +201,10 @@ pub fn parse(argv: &[String]) -> Result<Parsed, String> {
             "format" => format = Some(value),
             "quiet" => a.quiet = true,
             "verbose" => a.verbose = true,
+            "direct" => a.direct = true,
+            "idle" => a.idle_timeout = Some(parse_idle(&value)?),
+            "group" => a.group = Some(parse_group(&value)?),
+            "mode" => a.mode = Some(parse_mode(&value)?),
             _ => delimiter = Some(value),
         }
     }
```

Apply:

```diff
--- a/src/cli/mod.rs
+++ b/src/cli/mod.rs
@@ -13,6 +13,7 @@ pub mod args;
 mod maint;
 pub(crate) mod output;
 mod read;
+mod serve;
 mod table;
 mod width_table;
 mod write;
@@ -49,7 +50,8 @@ operations:
   popitem STORE [first|last]   pop the first or last live key (default: last)
   setdefault STORE KEY VALUE [VALUE ...]
                                print the row of KEY, setting it to the VALUEs if missing
-  serve STORE, stop STORE      not provided by this tsvz (exit 1)
+  serve STORE                  run a write handler for STORE in the foreground (§21)
+  stop STORE                   stop the handler of STORE
 
 options:
   -h, --help                   show this help
@@ -58,6 +60,9 @@ options:
   -q, --quiet                  print errors only
   -v, --verbose                print informational messages
   -d, --delimiter D            delimiter of a .tsv-family file: tab, comma, pipe, null or one character
+  --x-direct                   work on the files even when a handler serves the store
+  --x-idle-timeout S           serve: stop after S seconds without connections
+  --x-group G, --x-mode M      serve: let group G connect / set the socket's octal mode
   --                           every argument after this is positional
 
 exit status: 0 done, 1 failed or refused, 2 usage error, 3 missing key, 4 checksum mismatch
@@ -199,13 +204,8 @@ impl Ctx {
             "setdefault" => write::setdefault(self),
             "clear" => maint::run(self, true),
             "scrub" => maint::run(self, false),
-            "serve" | "stop" => {
-                diag!(
-                    "tsvz: {}: this tsvz does not provide a handler (§21); work on the files instead",
-                    self.args.op
-                );
-                Ok(1)
-            }
+            "serve" => serve::serve(self),
+            "stop" => serve::stop(self),
             other => {
                 diag!("tsvz: unknown operation {other}");
                 Ok(2)
```

- [ ] **Step 4: Run the tests to see them pass**

Run: `cargo test --all-features 2>&1 | grep -E '^test result|FAILED|panicked'; cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -1`
Expected: the 12 handler tests pass; 199 passed in total, 0 failed (the CLI tests are 29 now: one obsolete test went); no clippy warning.

- [ ] **Step 5: Run the conformance suite's socket mode**

```bash
cargo build --release
python3 ../TSVZ/tsvzconform.py --impl "$PWD/target/release/tsvz" --class reader,writer,cli,handler --mode files,socket \
  --feature bzip2,xz,zstd,crc32,crc32c,sha256,blake3,file-first,handler | tail -8
```

Expected: no FAIL; `conformant: reader ✓   writer ✓   cli ✓   handler ✓`. The forwarded mode waits for Task 9.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "tsvz serve and stop (§21.13): the socket, one thread per connection, signals and the idle timeout; --x-idle-timeout, --x-group, --x-mode and --x-direct.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_0198xw4TRnucPbiyi9csx4uj"
```

---

### Task 8: The Rust client (`handler::Client`)

A public `Client` holds one connection to a store's handler. Typed calls (`read`, `get`, `has`, `len`, `keys`, `set`, `delete`, `pop`, `popitem`, `setdefault`, `clear`, `scrub`, `verify`, `stop`) return decoded rows, key first; statuses 0, 3 and 4 are answers; 1 and 2 become `Error::Handler`; a broken connection is `Error::HandlerLost` (the request may or may not have been applied). Writes carry `--x-written`, and `--sync` when asked. `request` is the raw form. The client never falls back to the files.

**Files:**
- Create: `src/handler/client.rs`
- Modify: `src/handler/mod.rs`, `src/error.rs`
- Test: `tests/handler.rs`

**Interfaces:**
- Consumes: `pointer::{lookup, connect}`, `protocol::{Response, field, decode, decode_fields, read_response}`, `ops::protocol_record`.
- Produces: `tsvz::handler::Client` with `connect(store) -> Result<Client, Error>`, `sync(self, bool)`, `timeout(self, Option<Duration>)`, `warnings(self, Option<WarnFn>)`, `request(&mut self, fields: &[&str], bulk: Option<&[Record]>) -> Result<Response, Error>`, the typed calls of the spec's section 8, and `#[doc(hidden)] over(stream: UnixStream, store: &Path) -> Result<Client, Error>` and `send(&mut self, line: &str, bulk: Option<&[String]>) -> Result<Response, Error>` for Task 9. `Error` gains `NoHandler(String)`, `Handler { status: i32, message: String }`, `HandlerLost(String)`.

- [ ] **Step 1: Write the failing tests**

Apply:

```diff
--- a/tests/handler.rs
+++ b/tests/handler.rs
@@ -351,3 +351,103 @@ fn a_stale_pointer_is_taken_over() {
     let _served = Served::start(&s, &[]);
     assert_eq!(ask(&s, "len").lines, ["1"]);
 }
+
+fn strings(v: &[&str]) -> Vec<String> {
+    v.iter().map(|s| s.to_string()).collect()
+}
+
+#[test]
+fn the_rust_client_speaks_to_a_handler() {
+    use tsvz::handler::Client;
+    use tsvz::{Error, Record};
+    let dir = tempfile::tempdir().unwrap();
+    let s = store(dir.path(), "a\t1\n#_defaults_#\td\n");
+    let mut served = Served::start(&s, &[]);
+    let mut c = Client::connect(&s).unwrap().sync(true);
+    let data = |k: &str, v: &str| Record::Data {
+        key: k.into(),
+        values: vec![v.into()],
+    };
+    c.set(&[data("#k", "x\ty")]).unwrap();
+    assert_eq!(c.get("#k").unwrap(), Some(strings(&["#k", "x\ty"])));
+    assert_eq!(
+        c.get("zz").unwrap(),
+        None,
+        "a missing key, even with defaults"
+    );
+    assert!(c.has("a").unwrap() && !c.has("zz").unwrap());
+    assert_eq!(c.len().unwrap(), 2);
+    assert_eq!(c.keys().unwrap(), strings(&["a", "#k"]));
+    c.set(&[data("b", "2"), data("c", "3")]).unwrap();
+    assert_eq!(c.pop("b").unwrap(), Some(strings(&["b", "2"])));
+    assert_eq!(c.pop("b").unwrap(), None);
+    assert_eq!(c.popitem(true).unwrap(), Some(strings(&["c", "3"])), "last");
+    assert_eq!(
+        c.popitem(false).unwrap(),
+        Some(strings(&["a", "1"])),
+        "first"
+    );
+    assert_eq!(c.setdefault("n", &["v"]).unwrap(), strings(&["n", "v"]));
+    c.delete(&["n"]).unwrap();
+    assert_eq!(c.read().unwrap(), [strings(&["#k", "x\ty"])]);
+    assert!(c.verify().unwrap().is_empty());
+    c.scrub().unwrap();
+    assert_eq!(
+        c.request(&["get"], None).unwrap().status,
+        2,
+        "raw requests keep their status"
+    );
+    match c.setdefault("k", &[]) {
+        Err(Error::Handler { status: 2, .. }) => {}
+        other => panic!("setdefault without a value: {other:?}"),
+    }
+    c.clear().unwrap();
+    assert_eq!(c.len().unwrap(), 0);
+    c.stop().unwrap();
+    assert_eq!(served.wait(), 0);
+    assert!(matches!(Client::connect(&s), Err(Error::NoHandler(_))));
+}
+
+#[test]
+fn many_clients_at_once_all_get_their_answers() {
+    use tsvz::handler::Client;
+    let dir = tempfile::tempdir().unwrap();
+    let s = store(dir.path(), "");
+    let _served = Served::start(&s, &[]);
+    let threads: Vec<_> = (0..32)
+        .map(|t| {
+            let s = s.clone();
+            std::thread::spawn(move || {
+                let mut c = Client::connect(&s).unwrap();
+                for i in 0..50 {
+                    let key = format!("k{t}-{i}");
+                    c.set(&[tsvz::Record::Data {
+                        key: key.clone(),
+                        values: vec![i.to_string()],
+                    }])
+                    .unwrap();
+                    assert_eq!(c.get(&key).unwrap(), Some(vec![key, i.to_string()]));
+                }
+            })
+        })
+        .collect();
+    for t in threads {
+        t.join().unwrap();
+    }
+    assert_eq!(Client::connect(&s).unwrap().len().unwrap(), 32 * 50);
+}
+
+#[test]
+fn a_client_whose_handler_dies_reports_the_request_as_lost() {
+    use tsvz::handler::Client;
+    let dir = tempfile::tempdir().unwrap();
+    let s = store(dir.path(), "a\t1\n");
+    let mut served = Served::start(&s, &[]);
+    let mut c = Client::connect(&s).unwrap();
+    assert_eq!(c.len().unwrap(), 1);
+    served.child.kill().unwrap();
+    served.child.wait().unwrap();
+    let err = c.len().unwrap_err();
+    assert!(matches!(err, tsvz::Error::HandlerLost(_)), "{err:?}");
+    assert!(err.to_string().contains("may or may not"), "{err}");
+}
```

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test --all-features --test handler 2>&1 | grep -E '^error' | sort | uniq -c | head -5`
Expected: compile errors: `unresolved import tsvz::handler::Client`, `no variant named NoHandler` / `HandlerLost`.

- [ ] **Step 3: Implement the client**

Create `src/handler/client.rs`:

````rust
//! A Rust program's connection to a store's handler (§21).

use std::io::{BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::handler::pointer;
use crate::handler::protocol::{Response, decode, decode_fields, field, read_response};
use crate::ops::protocol_record;
use crate::{Error, Record, WarnFn, Warning};

/// One connection to the handler of a store: each call is one request (§21.3.2), answered with
/// the store as its files are when the handler receives it (§21.8).
///
/// Rows are returned key first, then their cells. Statuses 0, 3 (missing key) and 4 (checksum
/// mismatch) are answers; a request that fails (1) or is refused as a usage error (2) is
/// [`Error::Handler`]; a connection that breaks is [`Error::HandlerLost`]. A client never
/// falls back to the files: open a [`crate::Store`] for that.
///
/// ```no_run
/// use tsvz::handler::Client;
/// use tsvz::Record;
///
/// let mut c = Client::connect("data.tsvz")?;
/// c.set(&[Record::Data { key: "k".into(), values: vec!["v".into()] }])?;
/// assert_eq!(c.get("k")?, Some(vec!["k".to_string(), "v".to_string()]));
/// # Ok::<(), tsvz::Error>(())
/// ```
pub struct Client {
    store: PathBuf,
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    sync: bool,
    warn: Option<WarnFn>,
}

fn lost(e: std::io::Error) -> Error {
    Error::HandlerLost(e.to_string())
}

impl Client {
    /// Connect to the handler of `store`, which its pointer file names (§21.2.4).
    pub fn connect(store: impl AsRef<Path>) -> Result<Client, Error> {
        let store = store.as_ref();
        let info = match pointer::lookup(store) {
            Ok(Some(info)) => info,
            Ok(None) => return Err(Error::NoHandler("no handler is running".into())),
            Err(why) => return Err(Error::NoHandler(why)),
        };
        let stream = pointer::connect(&info, None).map_err(|e| Error::NoHandler(e.message))?;
        Client::over(stream, store)
    }

    /// A client over a connection already made to the handler of `store`.
    #[doc(hidden)]
    pub fn over(stream: UnixStream, store: &Path) -> Result<Client, Error> {
        let reader = stream.try_clone().map_err(lost)?;
        Ok(Client {
            store: store.to_path_buf(),
            reader: BufReader::new(reader),
            writer: stream,
            sync: false,
            warn: None,
        })
    }

    /// Have every write acknowledged only once it is on durable storage (`--sync`, §21.9.2).
    pub fn sync(mut self, on: bool) -> Client {
        self.sync = on;
        self
    }

    /// Wait at most `limit` for each answer (default: no limit); a timeout is
    /// [`Error::HandlerLost`].
    pub fn timeout(self, limit: Option<Duration>) -> Client {
        let _ = self.writer.set_read_timeout(limit);
        let _ = self.writer.set_write_timeout(limit);
        self
    }

    /// Where the handler's diagnostics (`#!` lines) go.
    pub fn warnings(mut self, warn: Option<WarnFn>) -> Client {
        self.warn = warn;
        self
    }

    /// Send one request line (fields already encoded), then `bulk` record lines ended by `#`,
    /// and read the whole response.
    #[doc(hidden)]
    pub fn send(&mut self, line: &str, bulk: Option<&[String]>) -> Result<Response, Error> {
        let mut text = String::with_capacity(line.len() + 1);
        text.push_str(line);
        text.push('\n');
        if let Some(records) = bulk {
            for record in records {
                text.push_str(record);
                text.push('\n');
            }
            text.push_str("#\n");
        }
        self.writer.write_all(text.as_bytes()).map_err(lost)?;
        read_response(&mut self.reader).map_err(lost)
    }

    /// The raw form: `fields` (operation and arguments, each sent §13-encoded with TAB), then
    /// `bulk` records ended by `#` (§21.7.2). The response keeps every status and line, such as
    /// the defaults row of a missing key, or the rows a read returned with status 1.
    pub fn request(&mut self, fields: &[&str], bulk: Option<&[Record]>) -> Result<Response, Error> {
        let line: Vec<String> = fields.iter().map(|f| field(f, false)).collect();
        let records: Option<Vec<String>> = bulk.map(|b| b.iter().map(protocol_record).collect());
        self.send(&line.join("\t"), records.as_deref())
    }

    /// A typed call: diagnostics go to the warnings callback, and statuses 1 and 2 are errors.
    fn call(&mut self, line: &str, bulk: Option<&[String]>) -> Result<Response, Error> {
        let response = self.send(line, bulk)?;
        if let Some(warn) = &self.warn {
            for text in &response.diagnostics {
                warn(&Warning {
                    part: self.store.clone(),
                    offset: None,
                    message: text.clone(),
                });
            }
        }
        match response.status {
            1 | 2 => Err(Error::Handler {
                status: response.status,
                message: response.message,
            }),
            _ => Ok(response),
        }
    }

    /// A write request's options: acknowledged once written, so that a failed write is an
    /// error; and once durable when `sync` is on.
    fn write(&self, rest: &str) -> String {
        let sync = if self.sync { "--sync\t" } else { "" };
        format!("--x-written\t{sync}{rest}")
    }

    fn first_row(response: Response) -> Option<Vec<String>> {
        if response.status != 0 {
            return None;
        }
        response.lines.first().map(|l| decode_fields(l))
    }

    /// Every live row (`read`).
    pub fn read(&mut self) -> Result<Vec<Vec<String>>, Error> {
        let r = self.call("read", None)?;
        Ok(r.lines.iter().map(|l| decode_fields(l)).collect())
    }

    /// The row of a live `key`; None when it is missing.
    pub fn get(&mut self, key: &str) -> Result<Option<Vec<String>>, Error> {
        let r = self.call(&format!("get\t{}", field(key, true)), None)?;
        Ok(Client::first_row(r))
    }

    pub fn has(&mut self, key: &str) -> Result<bool, Error> {
        Ok(self
            .call(&format!("has\t{}", field(key, true)), None)?
            .status
            == 0)
    }

    /// The number of live keys.
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&mut self) -> Result<usize, Error> {
        let r = self.call("len", None)?;
        r.lines
            .first()
            .and_then(|l| l.parse().ok())
            .ok_or_else(|| Error::Handler {
                status: 1,
                message: format!("len answered {:?}", r.lines),
            })
    }

    pub fn keys(&mut self) -> Result<Vec<String>, Error> {
        Ok(self
            .call("keys", None)?
            .lines
            .iter()
            .map(|l| decode(l))
            .collect())
    }

    /// Append `records` as one batch (§18.2): one record as `set`, several as `set -`.
    pub fn set(&mut self, records: &[Record]) -> Result<(), Error> {
        match records {
            [] => Ok(()),
            [one] => {
                let line = self.write(&format!("set\t{}", protocol_record(one)));
                self.call(&line, None).map(drop)
            }
            many => {
                let lines: Vec<String> = many.iter().map(protocol_record).collect();
                let line = self.write("set\t-");
                self.call(&line, Some(&lines)).map(drop)
            }
        }
    }

    /// Delete data keys (a marker is reset with [`Client::set`] and a `Record::Marker` without
    /// values).
    pub fn delete(&mut self, keys: &[&str]) -> Result<(), Error> {
        if keys.is_empty() {
            return Ok(());
        }
        let keys: Vec<String> = keys.iter().map(|k| field(k, true)).collect();
        let line = self.write(&format!("delete\t{}", keys.join("\t")));
        self.call(&line, None).map(drop)
    }

    /// The row of `key`, deleted; None when it is missing (§21.8.2: atomic).
    pub fn pop(&mut self, key: &str) -> Result<Option<Vec<String>>, Error> {
        let line = self.write(&format!("pop\t{}", field(key, true)));
        Ok(Client::first_row(self.call(&line, None)?))
    }

    /// The last (or first) live row, deleted; None when the store is empty.
    pub fn popitem(&mut self, last: bool) -> Result<Option<Vec<String>>, Error> {
        let which = if last { "last" } else { "first" };
        let line = self.write(&format!("popitem\t{which}"));
        Ok(Client::first_row(self.call(&line, None)?))
    }

    /// The row of `key`, set to `values` first when it is missing.
    pub fn setdefault(&mut self, key: &str, values: &[&str]) -> Result<Vec<String>, Error> {
        let mut line = format!("setdefault\t{}", field(key, true));
        for v in values {
            line.push('\t');
            line.push_str(&field(v, false));
        }
        let line = self.write(&line);
        let r = self.call(&line, None)?;
        Client::first_row(r).ok_or_else(|| Error::Handler {
            status: 1,
            message: "setdefault answered no row".into(),
        })
    }

    pub fn clear(&mut self) -> Result<(), Error> {
        let line = self.write("clear");
        self.call(&line, None).map(drop)
    }

    pub fn scrub(&mut self) -> Result<(), Error> {
        let line = self.write("scrub");
        self.call(&line, None).map(drop)
    }

    /// Checksum mismatches (§15.3): path, line, algorithm, expected, computed.
    pub fn verify(&mut self) -> Result<Vec<Vec<String>>, Error> {
        let r = self.call("verify", None)?;
        Ok(r.lines.iter().map(|l| decode_fields(l)).collect())
    }

    /// Ask the handler to make every acknowledged write durable and stop (§21.7.1).
    pub fn stop(mut self) -> Result<(), Error> {
        self.call("stop", None).map(drop)
    }
}
````

Apply:

```diff
--- a/src/error.rs
+++ b/src/error.rs
@@ -18,6 +18,14 @@ pub enum Error {
     BadPath(PathBuf),
     /// The operation was refused; nothing was written. The text says why.
     Refused(String),
+    /// No handler can be used for the store: none runs, or its pointer file or socket cannot be
+    /// trusted (§21.2.4). The text says why.
+    NoHandler(String),
+    /// The handler answered a request with status 1 (failed) or 2 (usage error) (§20.6, §21.6).
+    Handler { status: i32, message: String },
+    /// The connection to the handler broke during a request, which may or may not have been
+    /// applied.
+    HandlerLost(String),
 }
 
 impl Error {
@@ -48,6 +56,14 @@ impl fmt::Display for Error {
                 path.display()
             ),
             Error::Refused(why) => f.write_str(why),
+            Error::NoHandler(why) => write!(f, "no handler: {why}"),
+            Error::Handler { status, message } => {
+                write!(f, "the handler answered status {status}: {message}")
+            }
+            Error::HandlerLost(why) => write!(
+                f,
+                "lost the handler ({why}); the request may or may not have been applied"
+            ),
         }
     }
 }
```

Apply:

```diff
--- a/src/handler/mod.rs
+++ b/src/handler/mod.rs
@@ -1,6 +1,7 @@
 //! The write handler of §21: one process serves a store over a Unix-domain socket, so that many
 //! producers and command lines share one loaded store and one appender.
 
+mod client;
 #[doc(hidden)]
 pub mod engine;
 #[doc(hidden)]
@@ -10,4 +11,5 @@ pub mod protocol;
 #[doc(hidden)]
 pub mod server;
 
+pub use client::Client;
 pub use protocol::Response;
```

- [ ] **Step 4: Run the tests to see them pass**

Run: `cargo test --all-features 2>&1 | grep -E '^test result|FAILED|panicked'; cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -1; cargo doc --no-deps -q`
Expected: 15 handler tests and 2 doc tests pass; 203 passed in total, 0 failed; no clippy warning; the docs build without warning.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "handler::Client: a Rust program's connection to a store's handler; Error::NoHandler, Handler and HandlerLost.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_0198xw4TRnucPbiyi9csx4uj"
```

---

### Task 9: Routing command lines through a handler (§20.8)

Before working on the files, the CLI looks for a usable handler of a strict store: not for `serve`, `stop`, `verify` or `parts`, nor with `--x-direct`. An untrusted pointer is reported and ignored (§20.8.2); a handler on another host, or one this user may not reach, is skipped quietly; one that does not answer is reported. The request carries the arguments literally, each §13-encoded; writes carry `--x-written` so that a failed write exits 1 as on the files; bulk input is parsed in the store's variant and sent as protocol lines. The response is printed through the same `Printer` as on the files; `#!` lines go to standard error (hidden by `-q`); the status is the exit status. A read-only operation whose handler goes away mid-request runs on the files; any other exits 1. With `-v` the CLI names the handler it used.

**Files:**
- Create: `src/cli/route.rs`
- Modify: `src/cli/mod.rs` (`run` routes first, then `on_files`, then `finish`)
- Test: `tests/handler.rs`

**Interfaces:**
- Consumes: `pointer::{lookup, connect, path_of}`, `Client::{over, send}`, `protocol::{field, decode, decode_fields, Response}`, `ops::{Out, protocol_record}`, `cli::write::bulk`, `cli::output::Printer`.
- Produces: `cli::route::route(ctx: &mut Ctx) -> Option<Result<i32, Error>>` (None: work on the files).

- [ ] **Step 1: Write the failing tests**

Apply:

```diff
--- a/tests/handler.rs
+++ b/tests/handler.rs
@@ -451,3 +451,175 @@ fn a_client_whose_handler_dies_reports_the_request_as_lost() {
     assert!(matches!(err, tsvz::Error::HandlerLost(_)), "{err:?}");
     assert!(err.to_string().contains("may or may not"), "{err}");
 }
+
+/// (exit status, stdout, stderr) of a command line run in `dir`.
+fn cli(dir: &Path, args: &[&str], stdin: &[u8]) -> (i32, String, String) {
+    let out = run(dir, args, stdin);
+    (
+        out.status.code().unwrap_or(-1),
+        text(&out.stdout),
+        text(&out.stderr),
+    )
+}
+
+#[test]
+fn routed_commands_print_what_the_files_give() {
+    let dir = tempfile::tempdir().unwrap();
+    let d = dir.path();
+    fs::write(
+        d.join("s.csvz"),
+        "#_defaults_#,d1,d2\n<#>h,v<lt>,x<sep>y\na,1\nb,2\n",
+    )
+    .unwrap();
+    let _served = Served::start(&d.join("s.csvz"), &[]);
+    let reads: &[&[&str]] = &[
+        &["--format", "records", "read", "s.csvz"],
+        &["--format", "table", "read", "s.csvz"],
+        &["--format", "records", "get", "s.csvz", "a  ", "zz", "#h"],
+        &["--format", "records", "has", "s.csvz", "b"],
+        &["--format", "records", "has", "s.csvz", "zz"],
+        &["--format", "records", "len", "s.csvz"],
+        &["--format", "records", "keys", "s.csvz"],
+    ];
+    let same = |stage: &str| {
+        for args in reads {
+            let routed = cli(d, args, b"");
+            let mut direct_args = vec!["--x-direct"];
+            direct_args.extend_from_slice(args);
+            let direct = cli(d, &direct_args, b"");
+            assert_eq!(
+                (routed.0, &routed.1),
+                (direct.0, &direct.1),
+                "{stage}: {args:?}: {}",
+                routed.2
+            );
+        }
+    };
+    same("start");
+    let (code, _, err) = cli(d, &["-v", "set", "s.csvz", "c", "3,4"], b"");
+    assert_eq!(code, 0, "{err}");
+    assert!(err.contains("via the handler"), "{err}");
+    same("set");
+    assert_eq!(
+        cli(d, &["set", "s.csvz", "-"], b"e,5\n#_defaults_#,n\nf,6\n").0,
+        0
+    );
+    same("set -");
+    assert_eq!(cli(d, &["delete", "s.csvz", "a", "zz"], b"").0, 0);
+    assert_eq!(cli(d, &["delete", "s.csvz", "-"], b"e\n").0, 0);
+    same("delete");
+    let pop = cli(d, &["--format", "records", "pop", "s.csvz", "b"], b"");
+    assert_eq!((pop.0, pop.1.as_str()), (0, "b,2,d2\n"));
+    assert_eq!(cli(d, &["pop", "s.csvz", "b"], b"").0, 3);
+    let item = cli(
+        d,
+        &["--format", "records", "popitem", "s.csvz", "first"],
+        b"",
+    );
+    assert_eq!((item.0, item.1.as_str()), (0, "<#>h,v<lt>,x<sep>y\n"));
+    let sd = cli(
+        d,
+        &["--format", "records", "setdefault", "s.csvz", "g", "7"],
+        b"",
+    );
+    assert_eq!((sd.0, sd.1.as_str()), (0, "g,7,\n"));
+    assert_eq!(
+        cli(d, &["setdefault", "s.csvz", "#_defaults_#", "x"], b"").0,
+        2
+    );
+    same("pop, popitem, setdefault");
+    assert_eq!(cli(d, &["clear", "s.csvz"], b"").0, 0);
+    same("clear");
+    assert_eq!(cli(d, &["len", "s.csvz"], b"").1, "0\n");
+}
+
+#[test]
+fn an_unreadable_part_reads_the_same_through_the_handler() {
+    let dir = tempfile::tempdir().unwrap();
+    let d = dir.path();
+    let s = store(d, "a\t1\n");
+    fs::create_dir(d.join("s.tsvz.1")).unwrap();
+    fs::write(d.join("s.tsvz.2"), "b\t2\n").unwrap();
+    let _served = Served::start(&s, &[]);
+    let routed = cli(d, &["--format", "records", "read", "s.tsvz"], b"");
+    assert_eq!((routed.0, routed.1.as_str()), (1, "a\t1\nb\t2\n"));
+    assert!(routed.2.contains("s.tsvz.1"), "{}", routed.2);
+    assert_eq!(cli(d, &["pop", "s.tsvz", "b"], b"").0, 1);
+    assert_eq!(fs::read_to_string(d.join("s.tsvz.2")).unwrap(), "b\t2\n");
+}
+
+/// A trusted pointer to a socket of this test, whose "handler" reads one line and hangs up;
+/// the number of connections it took.
+fn rude_handler(store: &Path) -> std::sync::Arc<std::sync::atomic::AtomicUsize> {
+    use std::io::BufRead;
+    use std::os::unix::net::UnixListener;
+    use std::sync::atomic::{AtomicUsize, Ordering};
+    let dir = tempfile::Builder::new()
+        .prefix("rude")
+        .tempdir_in("/tmp")
+        .unwrap();
+    let path = dir.path().join("s");
+    let listener = UnixListener::bind(&path).unwrap();
+    let p = pointer::path_of(store);
+    fs::write(
+        &p,
+        format!(
+            "tsvz-handler\t1\naddress\tunix:{}\nhost\t{}\npid\t1\n",
+            path.display(),
+            pointer::hostname()
+        ),
+    )
+    .unwrap();
+    fs::set_permissions(&p, fs::Permissions::from_mode(0o644)).unwrap();
+    let count = std::sync::Arc::new(AtomicUsize::new(0));
+    let seen = count.clone();
+    std::thread::spawn(move || {
+        let _keep = dir;
+        for conn in listener.incoming() {
+            seen.fetch_add(1, Ordering::SeqCst);
+            let mut reader = std::io::BufReader::new(conn.unwrap());
+            let mut line = String::new();
+            let _ = reader.read_line(&mut line);
+        }
+    });
+    count
+}
+
+#[test]
+fn a_handler_that_hangs_up_falls_back_for_reads_and_fails_writes() {
+    use std::sync::atomic::Ordering;
+    let dir = tempfile::tempdir().unwrap();
+    let d = dir.path();
+    let s = store(d, "k\tv\n");
+    let contacts = rude_handler(&s);
+    let (code, out, err) = cli(d, &["--format", "records", "get", "s.tsvz", "k"], b"");
+    assert_eq!((code, out.as_str()), (0, "k\tv\n"), "{err}");
+    assert!(err.contains("lost the handler"), "{err}");
+    let (code, _, err) = cli(d, &["set", "s.tsvz", "n", "w"], b"");
+    assert_eq!(code, 1);
+    assert!(err.contains("may or may not"), "{err}");
+    assert_eq!(contacts.load(Ordering::SeqCst), 2);
+    // --x-direct, verify and parts never contact a handler; neither does a loose file.
+    assert_eq!(cli(d, &["--x-direct", "get", "s.tsvz", "k"], b"").0, 0);
+    assert_eq!(cli(d, &["verify", "s.tsvz"], b"").0, 0);
+    assert_eq!(cli(d, &["parts", "s.tsvz"], b"").0, 0);
+    fs::write(d.join("l.tsv"), "k\tv\n").unwrap();
+    fs::copy(pointer::path_of(&s), d.join("l.tsv.serve")).unwrap();
+    assert_eq!(cli(d, &["get", "l.tsv", "k"], b"").0, 0);
+    assert_eq!(contacts.load(Ordering::SeqCst), 2);
+}
+
+#[test]
+fn an_untrusted_pointer_is_ignored_with_a_warning() {
+    let dir = tempfile::tempdir().unwrap();
+    let d = dir.path();
+    let s = store(d, "k\tv\n");
+    let p = pointer::path_of(&s);
+    fs::write(&p, "tsvz-handler\t1\naddress\ttcp:127.0.0.1:9\nhost\tx\n").unwrap();
+    fs::set_permissions(&p, fs::Permissions::from_mode(0o600)).unwrap();
+    let (code, out, err) = cli(d, &["--format", "records", "get", "s.tsvz", "k"], b"");
+    assert_eq!((code, out.as_str()), (0, "k\tv\n"));
+    assert!(err.contains("ignoring the pointer file"), "{err}");
+    let (_, _, quiet) = cli(d, &["-q", "get", "s.tsvz", "k"], b"");
+    assert_eq!(quiet, "");
+}
```

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test --all-features --test handler 2>&1 | grep -E 'FAILED|test result'`
Expected: `16 passed; 3 failed`: `routed_commands_print_what_the_files_give` (no "via the handler"), `a_handler_that_hangs_up_falls_back_for_reads_and_fails_writes` and `an_untrusted_pointer_is_ignored_with_a_warning` (the CLI never looks at the pointer). `an_unreadable_part_reads_the_same_through_the_handler` passes already, on the files; it passes through the handler after Step 3.

- [ ] **Step 3: Implement routing**

Create `src/cli/route.rs`:

```rust
//! Forwarding a command line to the store's handler (§20.8).

use std::io::Write;

use crate::cli::Ctx;
use crate::cli::output::{Printer, stdout_error};
use crate::cli::write::bulk;
use crate::handler::protocol::{Response, decode, decode_fields, field};
use crate::handler::{Client, pointer};
use crate::ops::{Out, protocol_record};
use crate::{Error, Warning};

/// What a command line forwards. `verify` and `parts` read the files themselves and gain nothing
/// from a handler (TSVZ.py routes them the same way); `serve` and `stop` are not forwarded.
const ROUTED: &[&str] = &[
    "read",
    "get",
    "set",
    "append",
    "delete",
    "clear",
    "scrub",
    "has",
    "len",
    "keys",
    "pop",
    "popitem",
    "setdefault",
];
/// Of those, the ones redone on the files when the handler goes away during the request.
const READ_ONLY: &[&str] = &["read", "get", "has", "len", "keys"];

fn warn(ctx: &Ctx, message: String) {
    (ctx.warn)(&Warning {
        part: ctx.store.clone(),
        offset: None,
        message,
    });
}

/// Run the command line through the handler of a strict store when one can be used (§20.8.1):
/// Some(exit status), or None to work on the files (§20.8.2).
pub(crate) fn route(ctx: &mut Ctx) -> Option<Result<i32, Error>> {
    let op = ctx.args.op.clone();
    if ctx.args.direct || !ctx.format.strict || !ROUTED.contains(&op.as_str()) {
        return None;
    }
    let info = match pointer::lookup(&ctx.store) {
        Ok(Some(info)) => info,
        Ok(None) => return None,
        Err(why) => {
            warn(
                ctx,
                format!("ignoring the pointer file: {why}; using the files directly"),
            );
            return None;
        }
    };
    let stream = match pointer::connect(&info, None) {
        Ok(stream) => stream,
        Err(e) => {
            if !e.quiet {
                let at = pointer::path_of(&ctx.store);
                let why = e.message;
                warn(
                    ctx,
                    format!(
                        "the handler in {} does not answer ({why}); using the files directly",
                        at.display()
                    ),
                );
            }
            return None;
        }
    };
    ctx.info(&format!(
        "via the handler at {} (pid {})",
        info.address, info.pid
    ));
    let mut client = Client::over(stream, &ctx.store).ok()?;
    let writes = matches!(op.as_str(), "set" | "append" | "delete");
    // A write is answered once written, so that one that fails exits 1 as on the files.
    let mut line = String::from(if writes { "--x-written\t" } else { "" });
    line.push_str(&op);
    for arg in &ctx.args.args {
        line.push('\t');
        line.push_str(&field(arg, false)); // the arguments are literal (§20.2)
    }
    let records = if writes && ctx.args.args == ["-"] {
        match bulk(ctx, op == "delete") {
            Ok(records) => Some(records.iter().map(protocol_record).collect::<Vec<_>>()),
            Err(e) => return Some(Err(e)),
        }
    } else {
        None
    };
    match client.send(&line, records.as_deref()) {
        Ok(response) => Some(print(ctx, &response)),
        Err(e) if READ_ONLY.contains(&op.as_str()) => {
            warn(ctx, format!("{e}; using the files directly"));
            None
        }
        Err(e) => Some(Err(e)),
    }
}

/// Print a response as the operation prints on the files (§20.8.1).
fn print(ctx: &mut Ctx, response: &Response) -> Result<i32, Error> {
    if !ctx.args.quiet {
        for text in &response.diagnostics {
            diag!("tsvz: {text}");
        }
    }
    match ctx.args.op.as_str() {
        "read" | "get" | "pop" | "popitem" | "setdefault" => {
            let mut out = Printer::rows(ctx);
            for line in &response.lines {
                let fields = decode_fields(line);
                out.row(&fields[0], fields[1..].iter().map(String::as_str))?;
            }
            out.finish()?;
        }
        "keys" => {
            let mut out = Printer::rows(ctx);
            for line in &response.lines {
                out.row(&decode(line), std::iter::empty())?;
            }
            out.finish()?;
        }
        "len" => {
            for line in &response.lines {
                writeln!(ctx.out, "{line}").map_err(stdout_error)?;
            }
        }
        _ => {}
    }
    if matches!(response.status, 1 | 2) && !response.message.is_empty() {
        diag!("tsvz: {}: {}", ctx.store.display(), response.message);
    }
    Ok(response.status)
}
```

Apply:

```diff
--- a/src/cli/mod.rs
+++ b/src/cli/mod.rs
@@ -13,6 +13,7 @@ pub mod args;
 mod maint;
 pub(crate) mod output;
 mod read;
+mod route;
 mod serve;
 mod table;
 mod width_table;
@@ -189,7 +190,17 @@ impl Ctx {
     }
 
     fn run(&mut self) -> i32 {
-        let result = match self.args.op.as_str() {
+        let routed = route::route(self);
+        let result = match routed {
+            Some(result) => result,
+            None => self.on_files(),
+        };
+        self.finish(result)
+    }
+
+    /// Run the operation on the files (§20.2).
+    fn on_files(&mut self) -> Result<i32, Error> {
+        match self.args.op.as_str() {
             "read" => read::read(self),
             "get" => read::get(self),
             "has" => read::has(self),
@@ -210,7 +221,11 @@ impl Ctx {
                 diag!("tsvz: unknown operation {other}");
                 Ok(2)
             }
-        };
+        }
+    }
+
+    /// Report an error once and flush what was printed: the exit status.
+    fn finish(&mut self, result: Result<i32, Error>) -> i32 {
         let code = match result {
             Ok(code) => code,
             Err(e) => {
```

- [ ] **Step 4: Run the tests to see them pass**

Run: `cargo test --all-features 2>&1 | grep -E '^test result|FAILED|panicked'; cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -1`
Expected: 19 handler tests pass; 207 passed in total, 0 failed; no clippy warning.

- [ ] **Step 5: Run the whole conformance suite, every mode**

```bash
cargo build --release
python3 ../TSVZ/tsvzconform.py --impl "$PWD/target/release/tsvz" --class reader,writer,cli,handler \
  --feature bzip2,xz,zstd,crc32,crc32c,sha256,blake3,file-first,handler | tail -8
```

Expected: no FAIL; `conformant: reader ✓   writer ✓   cli ✓   handler ✓` (against TSVZ 4.7: reader 180, writer 43, cli 96, handler 599 passed). The `21.2.4-refuse-*` cli cases check that a command never connects to a pointer it must not trust.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "Route command lines through a running handler (§20.8): output and status as on the files; untrusted pointers are ignored with a warning; --x-direct works on the files.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_0198xw4TRnucPbiyi9csx4uj"
```

---

### Task 10: Working with TSVZ.py's handler and client

Both tools speak §21, plus `--x-written`: the tsvz-rs CLI must work through a TSVZ.py handler, TSVZ.py's CLI and `TSVZClient` through a tsvz-rs handler, and the two `serve`s must exclude each other through the pointer file's `flock`. These tests check Tasks 4–9 against the real TSVZ.py; they are expected to pass at once, and a failure is an interoperability bug to fix in the task that owns the code.

**Files:**
- Modify: `tests/interop.rs`

**Interfaces:**
- Consumes: `tsvz::handler::pointer::{lookup, connect}`; `python3`, `TSVZ_PY` (default `../TSVZ/TSVZ.py`).
- Produces: three interop tests.

- [ ] **Step 1: Write the tests**

Apply:

```diff
--- a/tests/interop.rs
+++ b/tests/interop.rs
@@ -194,3 +194,180 @@ fn stores_written_by_one_read_the_same_in_the_other() {
         }
     }
 }
+
+/// A handler of either tool, started in `dir`; killed if a test leaves it running.
+struct Handler(std::process::Child);
+
+impl Handler {
+    fn start(mut command: Command, store: &Path) -> Handler {
+        let child = command
+            .stdin(Stdio::null())
+            .stdout(Stdio::null())
+            .stderr(Stdio::piped())
+            .spawn()
+            .unwrap();
+        let handler = Handler(child);
+        let deadline = Instant::now() + Duration::from_secs(10);
+        loop {
+            let info = tsvz::handler::pointer::lookup(store).ok().flatten();
+            if info.is_some_and(|i| tsvz::handler::pointer::connect(&i, None).is_ok()) {
+                return handler;
+            }
+            assert!(
+                Instant::now() < deadline,
+                "no handler for {}",
+                store.display()
+            );
+            std::thread::sleep(Duration::from_millis(20));
+        }
+    }
+
+    fn rust(store: &Path, extra: &[&str]) -> Handler {
+        let mut c = Command::new(env!("CARGO_BIN_EXE_tsvz"));
+        c.arg("serve").arg(store).args(extra);
+        Handler::start(c, store)
+    }
+
+    fn python(py: &Path, store: &Path) -> Handler {
+        let mut c = Command::new("python3");
+        c.arg(py).arg("serve").arg(store);
+        Handler::start(c, store)
+    }
+
+    /// Wait for the handler to exit: its status and standard error.
+    fn finish(mut self) -> (Option<i32>, String) {
+        let deadline = Instant::now() + Duration::from_secs(10);
+        while self.0.try_wait().unwrap().is_none() {
+            assert!(Instant::now() < deadline, "the handler did not exit");
+            std::thread::sleep(Duration::from_millis(20));
+        }
+        let mut err = String::new();
+        use std::io::Read;
+        self.0
+            .stderr
+            .take()
+            .unwrap()
+            .read_to_string(&mut err)
+            .unwrap();
+        (self.0.wait().unwrap().code(), err)
+    }
+}
+
+impl Drop for Handler {
+    fn drop(&mut self) {
+        let _ = self.0.kill();
+        let _ = self.0.wait();
+    }
+}
+
+fn out(o: &Output) -> (Option<i32>, String) {
+    (
+        o.status.code(),
+        String::from_utf8_lossy(&o.stdout).into_owned(),
+    )
+}
+
+#[test]
+fn the_rust_cli_goes_through_a_python_handler() {
+    let py = need_tsvz_py!();
+    let dir = tempfile::tempdir().unwrap();
+    let store = dir.path().join("s.tsvz");
+    std::fs::write(&store, "a\t1\n").unwrap();
+    let s = store.to_str().unwrap();
+    let handler = Handler::python(&py, &store);
+    let mut verbose = Command::new(env!("CARGO_BIN_EXE_tsvz"));
+    verbose.args(["-v", "set", s, "k", "v"]);
+    let set = run(verbose, b"");
+    assert!(set.status.success());
+    assert!(String::from_utf8_lossy(&set.stderr).contains("via the handler"));
+    assert_eq!(
+        out(&rust(&["get", s, "k", "zz"], b"")),
+        (Some(3), "k\tv\nzz\t\n".into())
+    );
+    assert_eq!(
+        rust(&["set", s, "-"], b"b\t2\nc\t3\n").status.code(),
+        Some(0)
+    );
+    assert_eq!(
+        out(&rust(&["pop", s, "b"], b"")),
+        (Some(0), "b\t2\n".into())
+    );
+    assert_eq!(
+        out(&rust(&["popitem", s, "first"], b"")),
+        (Some(0), "a\t1\n".into())
+    );
+    assert_eq!(
+        out(&rust(&["setdefault", s, "n", "w"], b"")),
+        (Some(0), "n\tw\n".into())
+    );
+    assert_eq!(rust(&["delete", s, "c"], b"").status.code(), Some(0));
+    assert_eq!(out(&rust(&["keys", s], b"")), (Some(0), "k\nn\n".into()));
+    assert_eq!(out(&rust(&["len", s], b"")), (Some(0), "2\n".into()));
+    assert_eq!(rust(&["stop", s], b"").status.code(), Some(0));
+    let (code, err) = handler.finish();
+    assert_eq!(code, Some(0), "{err}");
+    assert_eq!(
+        std::fs::read_to_string(&store).unwrap().lines().last(),
+        Some("c")
+    );
+}
+
+#[test]
+fn python_commands_and_its_client_go_through_a_rust_handler() {
+    let py = need_tsvz_py!();
+    let dir = tempfile::tempdir().unwrap();
+    let store = dir.path().join("s.tsvz");
+    let s = store.to_str().unwrap();
+    let handler = Handler::rust(&store, &["-v"]);
+    assert_eq!(
+        python(&py, &["set", s, "k", "v"], b"").status.code(),
+        Some(0)
+    );
+    assert_eq!(
+        out(&python(&py, &["get", s, "k"], b"")),
+        (Some(0), "k\tv\n".into())
+    );
+    let script = "import sys\nsys.path.insert(0, sys.argv[1])\nimport TSVZ\n\
+        c = TSVZ.TSVZClient(sys.argv[2])\nc['a'] = 'x'\nprint(c['a'])\nprint(len(c))\n\
+        c.update({'b': 'y', 'c': 'z'})\nprint(c.pop('b'))\nprint(c.popitem())\n\
+        print(c.setdefault('d', 'w'))\ndel c['a']\nprint(c.keys())\nc.close()\n";
+    let mut client = Command::new("python3");
+    client
+        .arg("-c")
+        .arg(script)
+        .arg(py.parent().unwrap())
+        .arg(s);
+    let client = run(client, b"");
+    let stderr = String::from_utf8_lossy(&client.stderr);
+    assert!(client.status.success(), "{stderr}");
+    assert!(!stderr.contains("working on the files"), "{stderr}");
+    assert_eq!(
+        String::from_utf8_lossy(&client.stdout),
+        "['a', 'x']\n2\n['b', 'y']\n('c', ['c', 'z'])\n['d', 'w']\n['k', 'd']\n"
+    );
+    assert_eq!(python(&py, &["stop", s], b"").status.code(), Some(0));
+    let (code, err) = handler.finish();
+    assert_eq!(code, Some(0), "{err}");
+    assert!(
+        err.contains("set answered #0"),
+        "the requests came through the handler: {err}"
+    );
+}
+
+#[test]
+fn neither_tool_serves_a_store_the_other_serves() {
+    let py = need_tsvz_py!();
+    let dir = tempfile::tempdir().unwrap();
+    let store = dir.path().join("s.tsvz");
+    std::fs::write(&store, "").unwrap();
+    let s = store.to_str().unwrap();
+    let handler = Handler::rust(&store, &[]);
+    assert_eq!(python(&py, &["serve", s], b"").status.code(), Some(1));
+    assert_eq!(python(&py, &["stop", s], b"").status.code(), Some(0));
+    assert_eq!(handler.finish().0, Some(0));
+    let handler = Handler::python(&py, &store);
+    let second = rust(&["serve", s], b"");
+    assert_eq!(second.status.code(), Some(1));
+    assert_eq!(rust(&["stop", s], b"").status.code(), Some(0));
+    assert_eq!(handler.finish().0, Some(0));
+}
```

- [ ] **Step 2: Run them against TSVZ.py**

Run: `TSVZ_PY=$PWD/../TSVZ/TSVZ.py TSVZ_REQUIRE_INTEROP=1 cargo test --all-features --test interop 2>&1 | grep -E '^test |test result'`
Expected: 6 passed (3 old, 3 new), taking a second or more: without `TSVZ_REQUIRE_INTEROP` a missing TSVZ.py would skip them in 0.00 s, which proves nothing.

- [ ] **Step 3: Run everything**

Run: `TSVZ_PY=$PWD/../TSVZ/TSVZ.py TSVZ_REQUIRE_INTEROP=1 cargo test --all-features 2>&1 | grep -E '^test result|FAILED|panicked'`
Expected: 210 passed, 0 failed.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "Interop: each tool's commands through the other's handler, TSVZClient through tsvz serve, and neither serves a store the other serves.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_0198xw4TRnucPbiyi9csx4uj"
```

---

### Task 11: Robustness: random requests, a fuzz target, difftest through the handler

Random request lines and bulk records run through the parser and the engine without a panic (stable Rust, in the test suite); a cargo-fuzz target does the same for the parser, the bulk reader and the response reader on nightly; and difftest learns `--handler`, which sends tsvz-rs's operations through its own handler while TSVZ.py reads the files (`--x-direct`), so §20.8.1 is checked byte for byte on random damaged stores.

**Files:**
- Modify: `tests/robust.rs`, `fuzz/Cargo.toml`, `difftest/difftest.py`
- Create: `fuzz/fuzz_targets/request.rs`

**Interfaces:**
- Consumes: `Engine::{open, handle}`, `protocol::{parse, read_line, read_response}`, `ops::parse_records`, `tsvz serve`/`stop`.
- Produces: `random_requests_never_panic`; fuzz target `request`; `difftest.py --handler`.

- [ ] **Step 1: The random-request test**

Apply:

```diff
--- a/tests/robust.rs
+++ b/tests/robust.rs
@@ -125,3 +125,60 @@ fn random_arguments_never_panic() {
         let _ = tsvz::cli::args::parse(&args);
     }
 }
+
+#[test]
+fn random_requests_never_panic() {
+    use std::io::Cursor;
+    use tsvz::handler::engine::Engine;
+    use tsvz::handler::protocol::{parse, read_line, read_response};
+    let dir = tempfile::tempdir().unwrap();
+    let engine = Engine::open(&dir.path().join("s.tsvz"), None).unwrap();
+    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
+    const WORDS: &[&str] = &[
+        "read",
+        "get",
+        "set",
+        "append",
+        "delete",
+        "has",
+        "len",
+        "keys",
+        "pop",
+        "popitem",
+        "setdefault",
+        "verify",
+        "parts",
+        "clear",
+        "scrub",
+        "--sync",
+        "--x-written",
+        "-",
+        "first",
+        "#_defaults_#",
+        "#_strip_trailing_whites_#",
+        "<#>k",
+        "k ",
+        "k",
+        "",
+    ];
+    for _ in 0..2000 {
+        let mut text = Vec::new();
+        for _ in 0..1 + rng.below(4) {
+            text.extend_from_slice(WORDS[rng.below(WORDS.len())].as_bytes());
+            text.push(b'\t');
+        }
+        text.extend(rng.bytes(12));
+        text.push(b'\n');
+        let mut input = Cursor::new(text.clone());
+        while let Ok(Some(line)) = read_line(&mut input, 1 << 16) {
+            if let Ok(request) = parse(&line) {
+                let bulk: Vec<String> = (0..rng.below(3))
+                    .map(|_| String::from_utf8_lossy(&rng.bytes(16)).replace('\n', ""))
+                    .collect();
+                let bulk = request.bulk().then_some(&bulk[..]);
+                let _ = engine.handle(&request, bulk);
+            }
+        }
+        let _ = read_response(&mut Cursor::new(text));
+    }
+}
```

Run: `cargo test --all-features --test robust 2>&1 | grep -E 'test result|panicked'`
Expected: `test result: ok. 4 passed` (a panic here is a bug in Task 4 or 5: fix it there, with a test).

- [ ] **Step 2: The fuzz target**

Create `fuzz/fuzz_targets/request.rs`:

```rust
#![no_main]
//! Bytes as handler protocol input: request lines, the bulk records after them, and responses.

use std::io::Cursor;
use std::path::Path;

use libfuzzer_sys::fuzz_target;
use tsvz::handler::protocol::{parse, read_line, read_response};
use tsvz::name::Format;
use tsvz::ops::parse_records;

fuzz_target!(|data: &[u8]| {
    let mut input = Cursor::new(data);
    while let Ok(Some(line)) = read_line(&mut input, 1 << 16) {
        if let Ok(request) = parse(&line) {
            let _ = request.bulk();
        }
    }
    let tsvz = Format { delimiter: b'\t', strict: true };
    let _ = parse_records(data, tsvz, data.first() == Some(&b'd'), Path::new("fuzz"), None);
    let _ = read_response(&mut Cursor::new(data));
});
```

Apply:

```diff
--- a/fuzz/Cargo.toml
+++ b/fuzz/Cargo.toml
@@ -25,5 +25,12 @@ test = false
 doc = false
 bench = false
 
+[[bin]]
+name = "request"
+path = "fuzz_targets/request.rs"
+test = false
+doc = false
+bench = false
+
 [workspace]
 members = ["."]
```

Run: `(cd fuzz && cargo +nightly fuzz run request -- -max_total_time=60 2>&1 | grep -E 'Done|ERROR|panicked' | head -3)`
Expected: `Done N runs in 61 second(s)` with no crash (about a million runs).

- [ ] **Step 3: difftest through the handler**

Apply:

```diff
--- a/difftest/difftest.py
+++ b/difftest/difftest.py
@@ -12,8 +12,12 @@ rows to the store's width as TSVZ.py does (§3.6 lets a reader pad). `read`, `ve
 Loose files are left out: tsvz-rs reads them by design section 5.6's rule, TSVZ.py by its
 3.39 rules (§20.2.4 allows both; docs/silent-choices.md).
 
+With --handler, each seed's store is served by `tsvz serve`, so tsvz-rs's operations go through
+its handler (§20.8), while TSVZ.py still reads the files (`--x-direct`): what a handler answers
+must be what the files give (§20.8.1, §21.8.5).
+
 usage: python3 difftest/difftest.py --rust target/release/tsvz [--tsvz ../TSVZ/TSVZ.py]
-                                    [--seeds 0:200] [--keep DIR]
+                                    [--seeds 0:200] [--keep DIR] [--handler]
 Exit status: 0 when every seed agrees, 1 when one does not (its files are kept).
 """
 import argparse
@@ -23,9 +27,11 @@ import lzma
 import os
 import random
 import shutil
+import socket
 import subprocess
 import sys
 import tempfile
+import time
 import zlib
 
 try:
@@ -188,6 +194,42 @@ def ask(command, store, op):
     return (done.returncode, done.stdout)
 
 
+def serve(command, store):
+    """Start `tsvz serve STORE`; return the process once its pointer file names a socket that answers."""
+    proc = subprocess.Popen(command + ['-q', 'serve', store], stdin=subprocess.DEVNULL,
+                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
+    deadline = time.monotonic() + 10
+    while time.monotonic() < deadline:
+        if proc.poll() is not None:
+            raise RuntimeError('serve exited with status %d' % proc.returncode)
+        try:
+            with open(store + '.serve', encoding='utf-8') as f:
+                lines = f.read().split('\n')
+            fields = dict(line.split('\t', 1) for line in lines[1:] if '\t' in line)
+            address = fields.get('address', '')
+            if lines[0] == 'tsvz-handler\t1' and address.startswith('unix:'):
+                with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as probe:
+                    probe.connect(address[5:])
+                return proc
+        except OSError:
+            pass
+        time.sleep(0.02)
+    proc.kill()
+    proc.wait()
+    raise RuntimeError('no handler answered within 10 s')
+
+
+def unserve(command, store, proc):
+    """Stop the handler of STORE and reap it."""
+    subprocess.run(command + ['-q', 'stop', store], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
+                   timeout=60)
+    try:
+        proc.wait(timeout=30)
+    except subprocess.TimeoutExpired:
+        proc.kill()
+        proc.wait()
+
+
 # Differences where TSVZ.py, not tsvz-rs, departs from the spec: (reason, test). Remove an entry
 # once TSVZ.py is fixed. Each is listed in docs/silent-choices.md.
 KNOWN = []  # none as of TSVZ.py 4.6
@@ -204,10 +246,14 @@ def main():
     parser.add_argument('--tsvz', default=os.path.join(here, '..', '..', 'TSVZ', 'TSVZ.py'), help='TSVZ.py')
     parser.add_argument('--seeds', default='0:200', help='START:END, end excluded')
     parser.add_argument('--keep', help='where to keep the stores of seeds that disagree (default: a temporary directory)')
+    parser.add_argument('--handler', action='store_true',
+                        help="send tsvz-rs's operations through a tsvz-rs handler; TSVZ.py still reads the files")
     args = parser.parse_args()
     start, end = (int(x) for x in args.seeds.split(':'))
     rust = [os.path.abspath(args.rust)]
     python = [sys.executable, os.path.abspath(args.tsvz)]
+    if args.handler:
+        python.append('--x-direct')  # the files, not the handler that serves them
     keep = args.keep or tempfile.mkdtemp(prefix='difftest-')
     failed = 0
     seen = {}
@@ -219,6 +265,12 @@ def main():
         try:
             store, _ = make(rng, directory)
             problems = []
+            handler = None
+            if args.handler:
+                try:
+                    handler = serve(rust, store)
+                except RuntimeError as e:
+                    problems.append('  serve: %s' % e)
             for op in ops + [['table', 'read'], ['table', 'verify'], ['table', 'parts']]:
                 if op[0] == 'table':
                     a, b = ask_tty(rust, store, op[1:]), ask_tty(python, store, op[1:])
@@ -229,6 +281,8 @@ def main():
                     seen[known(op, a, b)] = seen.get(known(op, a, b), 0) + 1
                 elif not same:
                     problems.append('  %s: tsvz-rs %s %r\n  %s  TSVZ.py %s %r' % (' '.join(op), a[0], a[1][:300], ' ' * len(' '.join(op)), b[0], b[1][:300]))
+            if handler is not None:
+                unserve(rust, store, handler)
             if problems:
                 failed += 1
                 target = os.path.join(keep, 'seed%d' % seed)
```

```bash
cargo build --release
KEEP=$(mktemp -d)   # where seeds that disagree are kept (difftest is not a benchmark: /tmp is fine)
python3 difftest/difftest.py --rust target/release/tsvz --tsvz ../TSVZ/TSVZ.py --seeds 0:300 --keep "$KEEP"
python3 difftest/difftest.py --rust target/release/tsvz --tsvz ../TSVZ/TSVZ.py --seeds 0:200 --keep "$KEEP" --handler
```

Expected: `0 of 300 seeds disagree`, then `0 of 200 seeds disagree`. To confirm that `--handler` really routes, serve one store and run `target/release/tsvz -v read STORE`: standard error names the handler.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "Robustness: random requests never panic, a request fuzz target, and difftest --handler.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_0198xw4TRnucPbiyi9csx4uj"
```

---

### Task 12: Documentation and CI

The README gains a handler section and loses "except serve and stop"; the crate docs name `handler::Client`; `docs/silent-choices.md` gains a row for each choice of spec section 9; CI runs the handler class and difftest through the handler.

**Files:**
- Modify: `README.md`, `src/lib.rs`, `docs/silent-choices.md`, `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: everything above.
- Produces: documentation; CI that checks the handler.

- [ ] **Step 1: README**

Apply:

````diff
--- a/README.md
+++ b/README.md
@@ -25,9 +25,28 @@ tsvz verify people.tsvz
 tsvz --help
 ```
 
-Every operation of §20 is supported except `serve` and `stop`; the handler (§21) comes in a
-later release. Stores may be compressed (gzip, bzip2, xz, zstd) and split into parts; all four
-digests (crc32, crc32c, sha256, blake3) are verified.
+Every operation of §20 is supported. Stores may be compressed (gzip, bzip2, xz, zstd) and split
+into parts; all four digests (crc32, crc32c, sha256, blake3) are verified.
+
+## Handler
+
+`tsvz serve STORE` runs a write handler (§21) in the foreground: one process keeps the store
+loaded and appends every write, so that many producers share one appender. While it runs, every
+`tsvz` command on that store goes through it and prints what it would print on the files;
+`--x-direct` works on the files instead. `tsvz stop STORE` (or Ctrl-C, SIGTERM, SIGHUP) makes
+every acknowledged write durable and stops it.
+
+```bash
+tsvz serve people.tsvz &              # --x-idle-timeout 600 stops it after 10 idle minutes
+tsvz set people.tsvz bob Bob 41       # through the handler
+tsvz -v get people.tsvz bob           # -v names the handler it used
+tsvz stop people.tsvz
+```
+
+The handler and TSVZ.py's (`tsvz serve`, `TSVZClient`) speak the same protocol: either tool's
+commands go through either tool's handler. It serves strict stores (`.tsvz .csvz .nsvz .psvz`).
+Its socket is private to its user; `--x-group G` lets a group connect, `--x-mode M` sets the
+socket's mode. Rust programs talk to a handler with `tsvz::handler::Client`.
 
 ## Library
 
@@ -53,8 +72,9 @@ Build without the command line and C libraries with
 
 ## Conformance and compatibility
 
-tsvz-rs passes the reader, writer and cli classes of the TSVZ conformance suite with every
-codec and digest claimed, and is checked against TSVZ.py on random stores. Rust and Python
+tsvz-rs passes the reader, writer, cli and handler classes of the TSVZ conformance suite with
+every codec and digest claimed, and is checked against TSVZ.py on random stores, with and
+without its handler. Rust and Python
 writers can share a store: they take the same file locks. Where the specification leaves a
 choice open, tsvz-rs does what TSVZ.py does; [docs/silent-choices.md](docs/silent-choices.md)
 lists those choices. Benchmarks against TSVZ.py are in [bench/RESULTS.md](bench/RESULTS.md).
````

- [ ] **Step 2: Crate docs**

Apply:

```diff
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -5,8 +5,10 @@
 //! ## API
 //!
 //! The library's interface is what these docs show: [`Store`] to read, [`Writer`] and
-//! [`Record`] to append, [`maint`] to clear and scrub, and the types they use. The modules
-//! hidden from these docs (codecs, framing, replay, file names, locks, the command line) are
+//! [`Record`] to append, [`maint`] to clear and scrub, [`handler::Client`] to talk to a
+//! store's handler (§21), and the types they use. The modules
+//! hidden from these docs (codecs, framing, replay, file names, locks, operations, the
+//! handler's engine, protocol and server, the command line) are
 //! internals shared with the `tsvz` binary, the tests and the fuzz targets; they may change in
 //! any release.
 
```

- [ ] **Step 3: Silent choices**

Apply:

```diff
--- a/docs/silent-choices.md
+++ b/docs/silent-choices.md
@@ -27,6 +27,20 @@ TSVZ.py code it copies. The differential harness (`difftest/`) finds new ones.
 | Long options | A unique prefix is accepted (`--delim`), as argparse did | `_cliParseArgs` |
 | `setdefault` with a marker key | Usage error (2), as §21.7.3 asks of a handler | `_ServeStore` |
 | Bulk input | Lines with an empty key are skipped (a reader would ignore them) | `_cliStdinBatch` |
+| A part that cannot be read | Skipped with a warning: reads print what was read and exit 1 (§21.8.5 for a handler, and the same on the files so that routing changes nothing); `pop`, `popitem`, `setdefault`, `clear`, `scrub` and a `delete` that must name a key (§20.2.7) change nothing | `_specLoad` (`'unreadable'`) |
+| `--x-written` | A handler accepts it: the write is answered once written, and a failed write answers status 1. A routed `set`, `append` or `delete` sends it, so that a failed write exits 1 as on the files; so does every write of `handler::Client` | `_serveDispatch`, `_cliRouted`, `TSVZClient` |
+| Routed operations | Every operation but `serve`, `stop`, `verify` and `parts` (which read the files themselves) | `_cliHandler` |
+| A routed operation whose handler goes away | `read`, `get`, `has`, `len`, `keys` warn and run on the files; any other exits 1, saying it may or may not have been applied | `_cliRouted` |
+| Handler statuses | A reading operation on a missing store: 1, "no such store"; a read that skipped a part or stopped: 1, "could not read every part of the store", with what was read | `_serveDispatch` |
+| `pop`, `popitem`, `setdefault` through a handler | Answered once their write is made; a failed write answers 1 and no row | `_ServeStore.pop` |
+| Writes that fail after they were acknowledged | Reported on `serve`'s standard error; every response carries a `#!` warning until a batch is written again | `_ServeWriter`, `_serveRespond` |
+| The handler's socket | A new `tsvz-XXXXXX` directory under `$XDG_RUNTIME_DIR` when it is writable and access is private, else the temporary directory; socket 0600, 0660 with `--x-group`, or `--x-mode`; the directory 0700 plus search bits for whom the socket admits | `_ServeHandle.start` |
+| The pointer file | Created 0600, published 0644; one that is incomplete is read again after 50 ms; one with another first line, or no `address` or `host`, means no handler, without a warning | `_ServeHandle.lock`, `_serveLookup` |
+| Request line limit | 64 MiB; a longer line closes the connection | `_SERVE_LINE_LIMIT` |
+| `stop STORE` | After `#0`, waits up to 10 s until the pointer file no longer names that handler | `_cliStop` |
+| `serve` options | `--x-idle-timeout S`, `--x-group G`, `--x-mode M`, and `--x-direct` for any command, spelled as TSVZ.py spells them | `_CLI_OPTIONS` |
+| `-v` with a handler | A routed command names the handler it used; `serve -v` reports each request's status | — |
+| Strict stores only (departs from TSVZ.py) | `serve` refuses a loose file and the CLI never forwards one: tsvz-rs and TSVZ.py read loose files by different rules, so a forwarded command could print what the other tool's files would not | — |
 | Loose files | Design section 5.6's simple rule; TSVZ.py uses its 3.39 rules instead (§20.2.4 allows both), which also cut a row longer than the first to its width | — |
 | `-V` | `tsvz-rs 0.1.0` | — |
 
```

- [ ] **Step 4: CI**

Apply:

```diff
--- a/.github/workflows/ci.yml
+++ b/.github/workflows/ci.yml
@@ -31,5 +31,6 @@ jobs:
           TSVZ_PY: ${{ github.workspace }}/TSVZ/TSVZ.py
           TSVZ_REQUIRE_INTEROP: "1"
       - run: cargo build --release
-      - run: python3 TSVZ/tsvzconform.py --impl target/release/tsvz --class reader,writer,cli --feature bzip2,xz,zstd,crc32,crc32c,sha256,blake3,file-first
+      - run: python3 TSVZ/tsvzconform.py --impl target/release/tsvz --class reader,writer,cli,handler --feature bzip2,xz,zstd,crc32,crc32c,sha256,blake3,file-first,handler
       - run: python3 difftest/difftest.py --rust target/release/tsvz --tsvz TSVZ/TSVZ.py --seeds 0:200
+      - run: python3 difftest/difftest.py --rust target/release/tsvz --tsvz TSVZ/TSVZ.py --seeds 0:100 --handler
```

- [ ] **Step 5: Check**

Run: `cargo test --all-features --doc 2>&1 | grep 'test result'; cargo doc --no-deps -q 2>&1 | head -3; python3 -c "import yaml" 2>/dev/null && python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/ci.yml'))" && echo yaml-ok`
Expected: the doc tests pass (the README is the crate's front page and its Rust block compiles); no rustdoc warning; `yaml-ok` when PyYAML is installed.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "Docs: the handler in the README, its choices in silent-choices.md; CI runs the handler class and difftest --handler.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_0198xw4TRnucPbiyi9csx4uj"
```

---

### Task 13: Benchmarks with the handler

tsvzbench's `perop` family has three handler-related workloads: `cli` (single `set` and `get` commands), `cli via handler` (the same with a handler serving the store) and `socket` (requests straight to the handler). They were `n/a` for tsvz-rs; now they run. The whole standard suite is rerun too, since Task 2 touched every CLI path.

**Files:**
- Modify: `bench/RESULTS.md`

**Interfaces:**
- Consumes: `target/release/tsvz`, `../TSVZ/tsvzbench.py`, `../TSVZ/TSVZ.py`.
- Produces: `bench/RESULTS.md` with handler rows.

- [ ] **Step 1: Run the standard suite twice**

The work directory goes on `/mnt/gen5` (f2fs), never `/tmp` (tmpfs) or `/mnt/klein` (mergerfs).

```bash
cargo build --release
mkdir -p /mnt/gen5/tsvzbench-rs
for run in 1 2; do
  python3 ../TSVZ/tsvzbench.py --impl "py=python3 ../TSVZ/TSVZ.py" --impl "rs=target/release/tsvz" \
    --suite standard --only cold,bulk,perop,maint --work-dir /mnt/gen5/tsvzbench-rs \
    --json /mnt/gen5/tsvzbench-rs/run$run.json | tee /mnt/gen5/tsvzbench-rs/run$run.txt
done
```

Expected: `rs  ... handler: yes` in the implementations list; every `rs` cell faster than `py` (ratio below 1) in both runs, the `cli via handler` and `socket` rows included. Timings here can flip between a fast and a slow mode (about 25% at microsecond scales), so a ratio counts only when both runs agree.

- [ ] **Step 2: Fix any workload that is not faster**

Keep its fixture (`--keep`) and profile the slow side: for `socket`, `perf record -g -p $(pgrep -f 'tsvz serve') -- sleep 10` while the workload runs; for a command, `perf record -g -- target/release/tsvz …`. Then `perf report --stdio | head -60`. Likely costs, in order: the part listing `refresh` does when the directory is racy (younger than 2 s; the benchmark's own writes keep it so), opening the active part for every batch, and a response built line by line. Fix what the profile shows, keep the behaviour (add a test if the fix could change it), and rerun Step 1.

- [ ] **Step 3: Record the results**

`bench/RESULTS.md` keeps its layout (date; the commands of Step 1; machine; versions; both runs' tables; one sentence per family with the median ratio range). Replace its runs with the new ones, update the versions (TSVZ.py 4.7, tsvz-rs with the commit) and the summary, and drop the sentence that the handler rows are n/a.

- [ ] **Step 4: Commit**

```bash
git add bench/RESULTS.md
git commit -m "Bench: tsvz-rs against TSVZ.py with handlers, on the standard tsvzbench suite.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_0198xw4TRnucPbiyi9csx4uj"
```

---

### Task 14: Final review, merge, and the 0.1.0 release

The first public release ships the core crate (sub-project 1) and the handler together. It takes over Steps 3–5 of the core plan's Task 23, which were never run. Every outward step waits for the user's explicit yes.

**Files:**
- Modify: whatever the review finds.

**Interfaces:**
- Consumes: the finished branch.
- Produces: `handler` merged into `main`; with the user's yes, the GitHub repository, `tsvz` 0.1.0 on crates.io, and a `v0.1.0` release with four binaries.

- [ ] **Step 1: Final review of the whole branch**

Follow the executing skill's final review: one fresh reviewer, on the most capable model, over `git merge-base main HEAD`..`HEAD`, with this plan, the spec and the Review Focus. Re-grade the findings by effect; fix Critical and Important ones, each with a test that fails first; ledger the minors.

- [ ] **Step 2: Check the done list (spec section 13)**

- [ ] Conformance: reader, writer, cli and handler ✓ against TSVZ `main`, no FAIL, no unlisted warn (Task 9 Step 5; CI on Linux and macOS once the repository exists).
- [ ] difftest: `0 of N seeds disagree`, with and without `--handler` (Task 11 Step 3).
- [ ] `bench/RESULTS.md`: faster than TSVZ.py on every workload, handler ones included (Task 13).
- [ ] Unit, handler, interop (`TSVZ_REQUIRE_INTEROP=1`), robust and fuzz runs pass.
- [ ] `docs/silent-choices.md` and the README cover the handler.

- [ ] **Step 3: Merge**

Use superpowers:finishing-a-development-branch: run the suite, then the user chooses whether to merge `handler` into `main` locally, push it, or keep it.

- [ ] **Step 4: Create the GitHub repository and push (ask first)**

```bash
gh repo create yufei-pan/tsvz-rs --public --source . --remote origin --description "Fast Rust implementation of tsvz-spec-v1: the tsvz command-line tool, handler and library"
git push -u origin main
```

Expected: CI runs and passes on Linux and macOS, including the handler class, interop and both difftest runs.

- [ ] **Step 5: Publish the crate (ask first)**

The release-gate hook blocks `cargo publish` until the release is reviewed: run the code-review skill over the diff since the commit that set version 0.1.0 (Step 1's review covers the handler; the core was reviewed in its own plan), fix or surface what it finds, then `python3 /root/.claude/hooks/release_gate.py approve . --summary "…"` (`release_gate.py status .` shows the state). Then:

```bash
cargo publish --dry-run
cargo publish
```

Expected: `tsvz 0.1.0` on crates.io; `cargo install tsvz` installs the binary, and `tsvz serve` works from it.

- [ ] **Step 6: Tag the release (ask first)**

```bash
git tag -a v0.1.0 -m "tsvz-rs 0.1.0"
git push origin v0.1.0
```

Expected: the release workflow attaches `tsvz-v0.1.0-<target>` archives for the four targets.

- [ ] **Step 7: Clean up and report follow-ups**

Delete the core plan's workspace (`.superpowers/sdd/2026-10-12-tsvz-rs-core/`, its Task 23 is now complete) and this plan's. Report, without doing them: a Rust row in tsvz.org's implementations table (TSVZ repository); a tsvz-rs row in `cas/CLAUDE.md`; a TSVZ conformance case for an in-place rewrite followed by appends past the old size (TSVZ repository); sub-project 3 (multi-part scrub, §19.2).

---

## Self-review notes

- **Spec coverage.**
  - Section 1 (layout): Tasks 2–9.
  - Section 2.1 (unreadable parts): Task 1.
  - Section 2.2 (ops): Task 2.
  - Section 3 (protocol): Task 4.
  - Section 4 (engine): Task 5. §20.2.7 naming came after the spec, with TSVZ 4.7: Task 3, and the engine's `delete` in Task 5.
  - Sections 5–6 (pointer, connections, stopping): Tasks 6 and 7.
  - Section 7 (the command line): Tasks 7 and 9.
  - Section 8 (client): Task 8.
  - Section 9 (interoperability): Tasks 10 and 12.
  - Section 10 (testing): every task, plus Tasks 10–11.
  - Section 11 (performance): Task 13.
  - Sections 12–13 (docs, CI, release, done): Tasks 12 and 14.
- **Checked while planning.** Every code block of Tasks 1–12 was built from a prototype: the generated blocks were applied in order to `main`, and the result was compared with it. At every task boundary the whole suite passes, with the totals stated in the steps (149/150, 157, 162, 168, 182, 188, 199, 203, 207, 210, 211, 211). Clippy (`--all-targets --all-features -D warnings`) and rustfmt are clean. Against TSVZ 4.7 (TSVZ `main` 43ea176):
  - Full conformance: reader ✓, writer ✓, cli ✓ (96 passed), handler ✓ (599 passed).
  - difftest: 0 of 300 seeds disagree, and 0 of 200 with `--handler`.
  - The interop tests pass.
  - The request fuzz target ran 1.1 million inputs without a crash.
- **Deviation from the spec.** Spec section 4.4 said `set`, `append` and `delete` guard tombstones. That holds only for `delete`, and only through §20.2.7 (Task 3); `set` and `append` write as the CLI does. The spec was amended with this plan.
