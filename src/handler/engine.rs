//! One store's live state for its handler (§21.7–§21.9).
//!
//! The state follows the files: every write is appended by one writer thread and comes back
//! through `Store::refresh`, like any other writer's (§21.1.2). A request that reads waits until
//! every queued write is written, then refreshes, so it sees every write acknowledged before it
//! and every record committed to the files before it (§21.8.1), and what it answers is what a
//! reader of the files returns (§21.8.5). Requests are serialized by one mutex; acknowledgements
//! are waited for outside it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;

use crate::handler::protocol::{Request, Response};
use crate::maint::{self, Outcome};
use crate::name::{self, Format};
use crate::ops::{self, Out, Plan};
use crate::parts::Stamp;
use crate::write::{Record, Writer, encode_record};
use crate::{Error, Options, Scope, Store, WarnFn, Warning, escape, parts};

/// The message of a read operation on a store that does not exist (§20.2.2).
pub const NO_STORE: &str = "no such store";
/// The message of a read that skipped a part or stopped at damaged data (§21.8.5).
pub const INCOMPLETE: &str = "could not read every part of the store";

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

struct Entry {
    seq: u64,
    payload: Vec<u8>,
    durable: bool,
    /// A request waits for its outcome.
    waited: bool,
}

#[derive(Default)]
struct Queue {
    entries: Vec<Entry>,
    member_size: u64,
    submitted: u64,
    written: u64,
    /// The outcome of each waited entry once its batch is written (and fsync'd when durable),
    /// until its request takes it.
    outcomes: HashMap<u64, Result<(), String>>,
    /// The last batch's error, until a batch is written again.
    failing: Option<String>,
    /// The part the last batch went to and its stamp just after; None before the first batch
    /// and after a failed one.
    own: Option<(PathBuf, Stamp)>,
    /// How many batches found their part changed since the last one: other writers wrote.
    foreign: u64,
    unsynced: bool,
    closing: bool,
}

/// The single appender of the store (§18.3): each turn appends everything queued as one batch.
struct Batcher {
    store: PathBuf,
    queue: Mutex<Queue>,
    changed: Condvar,
    /// Warnings of the writer (a torn tail cut, …).
    log: Option<WarnFn>,
    /// Failed batches and fsyncs.
    errors: Option<WarnFn>,
}

impl Batcher {
    /// Queue `payload` (records encoded for the store, each ending in `\n`); its sequence number.
    /// A `waited` entry's outcome is kept for [`Batcher::wait`].
    fn submit(&self, payload: Vec<u8>, durable: bool, member_size: u64, waited: bool) -> u64 {
        let mut q = lock(&self.queue);
        q.submitted += 1;
        let seq = q.submitted;
        q.member_size = member_size;
        q.entries.push(Entry {
            seq,
            payload,
            durable,
            waited,
        });
        self.changed.notify_all();
        seq
    }

    /// Wait for the outcome of the waited entry `seq`: written (and fsync'd when durable), or
    /// its batch's error.
    fn wait(&self, seq: u64) -> Result<(), String> {
        let mut q = lock(&self.queue);
        loop {
            if let Some(outcome) = q.outcomes.remove(&seq) {
                return outcome;
            }
            q = self.changed.wait(q).unwrap_or_else(|e| e.into_inner());
        }
    }

    /// The last sequence number written (or failed), and the count of batches that found
    /// other writers' data.
    fn progress(&self) -> (u64, u64) {
        let q = lock(&self.queue);
        (q.written, q.foreign)
    }

    /// Whether the files may hold data of other writers that the last follow, which saw
    /// `foreign` such batches, did not: none when every batch since found its part as the last
    /// one left it, and the part and the directory are still so.
    fn others_wrote(&self, foreign: u64) -> bool {
        let (count, own) = {
            let q = lock(&self.queue);
            (q.foreign, q.own.clone())
        };
        count != foreign
            || own.is_none_or(|(part, at)| {
                std::fs::metadata(&part).map_or(true, |m| Stamp::of(&m) != at)
            })
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
            let last = batch[batch.len() - 1].seq;
            let mut payload = Vec::with_capacity(batch.iter().map(|e| e.payload.len()).sum());
            for e in &batch {
                payload.extend_from_slice(&e.payload);
            }
            let result = Writer::open(&self.store).and_then(|w| {
                w.member_size(member_size as usize)
                    .fsync(durable)
                    .warnings(self.log.clone())
                    .append_payload_at(&payload)
            });
            let mut q = lock(&self.queue);
            q.written = last;
            let outcome = result.as_ref().map(|_| ()).map_err(|e| e.to_string());
            for e in batch.iter().filter(|e| e.waited) {
                q.outcomes.insert(e.seq, outcome.clone());
            }
            match result {
                Ok(appended) => {
                    let ours = q
                        .own
                        .as_ref()
                        .is_some_and(|(part, at)| *part == appended.part && *at == appended.before);
                    if !ours {
                        q.foreign += 1;
                    }
                    q.own = Some((appended.part, appended.after));
                    q.failing = None;
                    if !durable {
                        q.unsynced = true;
                    }
                }
                Err(e) => {
                    q.own = None;
                    let message = e.to_string();
                    if let Some(errors) = &self.errors {
                        errors(&Warning {
                            part: self.store.clone(),
                            offset: None,
                            message: format!("a batch of {} writes failed: {message}", batch.len()),
                        });
                    }
                    q.failing = Some(message);
                }
            }
            self.changed.notify_all();
        }
        let unsynced = lock(&self.queue).unsynced;
        if unsynced {
            if let Err(e) = self.fsync() {
                if let Some(errors) = &self.errors {
                    errors(&Warning {
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
    /// The last `#_write_ack_#` and `#_member_size_#` values queued, with their sequence
    /// numbers: until the store follows the files past them, they govern later writes.
    queued_ack: Option<(u64, &'static str)>,
    queued_member_size: Option<(u64, u64)>,
    /// The last sequence number written when the store last followed the files, and the
    /// writer's count of batches that found other writers' data then.
    reflected: u64,
    foreign: u64,
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
/// The value of a queued marker the store has not followed yet (`reflected`: the last sequence
/// number written when it last did).
fn queued<T>(slot: Option<(u64, T)>, reflected: u64) -> Option<T> {
    slot.filter(|(seq, _)| *seq > reflected).map(|(_, v)| v)
}

struct Queued {
    seq: u64,
    /// The request waits for its outcome ([`Batcher::wait`]).
    waited: bool,
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
    /// every warning, `errors` every failed batch and fsync, for `serve`'s standard error.
    pub fn open(path: &Path, log: Option<WarnFn>, errors: Option<WarnFn>) -> Result<Engine, Error> {
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
            errors,
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
                queued_ack: None,
                queued_member_size: None,
                reflected: 0,
                foreign: 0,
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
        self.follow(st)
    }

    /// Follow the files as they are now, without waiting for queued writes.
    fn follow(&self, st: &mut State) -> Result<(), Error> {
        let (written, foreign) = self.batcher.progress(); // what the files hold at least
        let reopened = match &mut st.store {
            Some(store) => match store.refresh() {
                Ok(_) => {
                    (st.reflected, st.foreign) = (written, foreign);
                    return Ok(());
                }
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
        (st.reflected, st.foreign) = (written, foreign);
        Ok(())
    }

    /// Encode `records` for the store and queue them; None when there is nothing to write.
    /// Whether the write is made durable, and its member size, follow the markers as they stand
    /// where it will be appended: the files followed now, then the markers still queued. The
    /// request waits for a durable write, and for any when `written`.
    fn submit(
        &self,
        st: &mut State,
        records: &[Record],
        sync: bool,
        member_size: Option<u64>,
        written: bool,
    ) -> Result<Option<Queued>, Error> {
        let mut payload = String::new();
        for r in records {
            encode_record(r, self.format, &mut payload)?;
        }
        if payload.is_empty() {
            return Ok(None);
        }
        // The store's markers are current unless another writer wrote since it last followed
        // the files: then follow them (cheaply, without waiting for queued writes). A file
        // that cannot be followed leaves the state last read; reads report it.
        let others = self.batcher.others_wrote(st.foreign)
            || !st
                .store
                .as_mut()
                .is_some_and(|s| s.same_parts().unwrap_or(false));
        if others {
            let _ = self.follow(st);
        }
        let reflected = st.reflected;
        let markers = st.store.as_ref().map(Store::markers);
        let ack = queued(st.queued_ack, reflected).or(markers.map(|m| m.write_ack));
        let member_size = member_size
            .or(queued(st.queued_member_size, reflected))
            .or(markers.map(|m| m.member_size))
            .unwrap_or(crate::replay::DEFAULT_MEMBER_SIZE);
        let durable = sync || ack == Some("disk");
        let waited = durable || written;
        let seq = self
            .batcher
            .submit(payload.into_bytes(), durable, member_size, waited);
        for r in records {
            let Record::Marker { key, values } = r else {
                continue;
            };
            let key = key.to_ascii_lowercase();
            let value = values.first().map_or("", String::as_str);
            let mut m = crate::Markers::default();
            if !crate::replay::apply_marker(&mut m, &key, value) {
                continue; // an invalid value changes nothing (§12.5)
            }
            match key.as_str() {
                "#_write_ack_#" => st.queued_ack = Some((seq, m.write_ack)),
                "#_member_size_#" => st.queued_member_size = Some((seq, m.member_size)),
                _ => {}
            }
        }
        Ok(Some(Queued { seq, waited }))
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
                return self.submit(st, &records, req.sync, None, req.written);
            }
            self.submit(st, &records, req.sync, None, req.written)
        });
        let result = result.and_then(|queued| match queued {
            Some(q) if q.waited => self.batcher.wait(q.seq).map_err(Error::Refused),
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
                    let queued = self.submit(st, &records, req.sync, Some(member_size), true)?;
                    (Engine::row(&key, &cells), queued)
                }
                Plan::Put {
                    key,
                    cells,
                    record,
                    member_size,
                } => {
                    let queued = self.submit(st, &[record], req.sync, Some(member_size), true)?;
                    (Engine::row(&key, &cells), queued)
                }
            })
        });
        // The row is answered only once its write is made (TSVZ.py's handler).
        let result = result.and_then(|(response, queued)| match queued {
            Some(q) => self
                .batcher
                .wait(q.seq)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    use crate::handler::protocol::parse;

    fn engine(dir: &Path, name: &str) -> Engine {
        Engine::open(&dir.join(name), None, None).unwrap()
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
        assert!(Engine::open(&dir.path().join("s.tsv"), None, None).is_err());
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
    fn write_ack_and_member_size_follow_other_writers_and_markers_not_yet_written() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(&p, "a\t1\n").unwrap();
        let e = engine(dir.path(), "s.tsvz");
        let submit = |records: &[Record]| {
            let mut st = lock(&e.state);
            // Not `written`: a write is waited for exactly when it is durable.
            e.submit(&mut st, records, false, None, false)
                .unwrap()
                .unwrap()
        };
        let data = || {
            [Record::Data {
                key: "k".into(),
                values: vec!["v".into()],
            }]
        };
        let marker = |key: &str, value: &str| {
            [Record::Marker {
                key: key.into(),
                values: vec![value.into()],
            }]
        };
        // Another writer asks for acknowledgements on disk: the next write is made durable.
        fs::OpenOptions::new()
            .append(true)
            .open(&p)
            .unwrap()
            .write_all(b"#_write_ack_#\tdisk\n")
            .unwrap();
        assert!(submit(&data()).waited, "a marker another writer appended");
        // Markers queued but not yet written (the writer waits for the lock) govern what follows.
        let file = fs::OpenOptions::new().append(true).open(&p).unwrap();
        let held = crate::lock::lock(&file, &p).unwrap();
        submit(&marker("#_WRITE_ACK_#", "Memory"));
        submit(&marker("#_member_size_#", "7"));
        assert!(!submit(&data()).waited, "#_write_ack_# memory, queued");
        assert_eq!(lock(&e.batcher.queue).member_size, 7);
        submit(&marker("#_member_size_#", "zero")); // invalid: ignored, as a reader ignores it
        submit(&data());
        assert_eq!(lock(&e.batcher.queue).member_size, 7);
        drop(held);
        assert_eq!(ask(&e, "get\tk").lines, ["k\tv"]);
    }

    #[test]
    fn a_marker_another_writer_appends_between_two_of_ours_is_seen() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(&p, "a\t1\n").unwrap();
        // A directory last changed long ago, whose timestamp shows a new part: the engine then
        // trusts its own bookkeeping to tell whether another writer wrote.
        let long_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
        fs::File::open(dir.path())
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(long_ago))
            .unwrap();
        let e = engine(dir.path(), "s.tsvz");
        let data = |k: &str| {
            [Record::Data {
                key: k.into(),
                values: vec!["v".into()],
            }]
        };
        let submit = |records: &[Record]| {
            let mut st = lock(&e.state);
            // Not `written`: a write is waited for exactly when it is durable.
            e.submit(&mut st, records, false, None, false)
                .unwrap()
                .unwrap()
        };
        assert!(!submit(&data("x")).waited);
        e.batcher.barrier();
        assert!(!submit(&data("y")).waited);
        let file = fs::OpenOptions::new().append(true).open(&p).unwrap();
        let held = crate::lock::lock(&file, &p).unwrap(); // y waits for the lock
        assert!(!submit(&data("z")).waited);
        (&file).write_all(b"#_write_ack_#\tdisk\n").unwrap(); // before y is written
        drop(held);
        e.batcher.barrier();
        assert!(
            submit(&data("w")).waited,
            "written after y, but before it on file"
        );
        e.batcher.barrier();
        // After our last append: the part is no longer as we left it.
        (&file).write_all(b"#_write_ack_#\tmemory\n").unwrap();
        assert!(!submit(&data("v")).waited, "appended after w");
        assert_eq!(ask(&e, "keys").lines, ["a", "x", "y", "z", "w", "v"]);
        // A new part: the directory changes.
        fs::write(dir.path().join("s.tsvz.1"), "#_write_ack_#\tdisk\n").unwrap();
        assert!(submit(&data("u")).waited, "a part another writer added");
        e.batcher.barrier();
        assert_eq!(
            fs::read_to_string(dir.path().join("s.tsvz.1")).unwrap(),
            "#_write_ack_#\tdisk\nu\tv\n"
        );
    }

    #[test]
    fn its_own_markers_govern_until_it_follows_the_files_past_them() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(&p, "a\t1\n").unwrap();
        let long_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
        fs::File::open(dir.path())
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(long_ago))
            .unwrap();
        let e = engine(dir.path(), "s.tsvz");
        let submit = |records: &[Record]| {
            let mut st = lock(&e.state);
            // Not `written`: a write is waited for exactly when it is durable.
            e.submit(&mut st, records, false, None, false)
                .unwrap()
                .unwrap()
        };
        let data = [Record::Data {
            key: "k".into(),
            values: vec!["v".into()],
        }];
        submit(&data);
        e.batcher.barrier();
        submit(&data); // follows once after the first batch, then trusts its bookkeeping
        e.batcher.barrier();
        submit(&[Record::Marker {
            key: "#_write_ack_#".into(),
            values: vec!["disk".into()],
        }]);
        e.batcher.barrier(); // written, but the store has not followed the files since
        assert!(submit(&data).waited);
    }

    #[test]
    fn a_failed_write_is_never_acknowledged_however_many_fail_after_it() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.tsvz");
        fs::write(&p, "a\t1\n").unwrap();
        let e = engine(dir.path(), "s.tsvz");
        fs::remove_file(&p).unwrap();
        fs::create_dir(&p).unwrap(); // every append fails, even as root
        let submit = |k: String, written: bool| {
            let mut st = lock(&e.state);
            let data = [Record::Data {
                key: k,
                values: vec!["v".into()],
            }];
            e.submit(&mut st, &data, false, None, written)
                .unwrap()
                .unwrap()
        };
        let first = submit("k0".into(), true);
        e.batcher.barrier();
        for i in 1..=100 {
            submit(format!("k{i}"), i % 2 == 0); // a batch of its own, failing too
            e.batcher.barrier();
        }
        assert!(e.batcher.wait(first.seq).is_err(), "its batch failed");
        let q = lock(&e.batcher.queue);
        assert_eq!(q.outcomes.len(), 50, "kept only for the requests that wait");
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
        assert_eq!(
            r.status, 0,
            "the parts before the active one, compacted: {r:?}"
        );
        assert_eq!(ask(&e, "get\tb").lines, ["b\t2"]);
        let r = ask(&e, "parts");
        assert_eq!(r.lines.len(), 2);
        assert!(r.lines[1].ends_with("s.tsvz.1\tactive"), "{r:?}");
        assert_eq!(ask(&e, "scrub").status, 0, "and again");
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
