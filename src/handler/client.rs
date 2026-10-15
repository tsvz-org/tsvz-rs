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
/// [`Error::Handler`]; a connection that breaks or times out is [`Error::HandlerLost`], and
/// so is every later call on it: connect again. A client never falls back to the files: open
/// a [`crate::Store`] for that.
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
    /// Why the connection was given up: after a send or an answer fails, the stream may hold
    /// half a request or a late answer, so it is never used again.
    lost: Option<String>,
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
            lost: None,
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
        if let Some(why) = &self.lost {
            return Err(Error::HandlerLost(format!(
                "the connection was lost earlier ({why})"
            )));
        }
        let answer = self
            .writer
            .write_all(text.as_bytes())
            .and_then(|()| read_response(&mut self.reader));
        answer.map_err(|e| {
            self.lost = Some(e.to_string());
            let _ = self.writer.shutdown(std::net::Shutdown::Both);
            lost(e)
        })
    }

    /// The raw form: `fields` (operation and arguments, each sent §13-encoded with TAB), then
    /// `bulk` records ended by `#` (§21.7.2). The response keeps every status and line, such as
    /// the defaults row of a missing key, or the rows a read returned with status 1.
    pub fn request(&mut self, fields: &[&str], bulk: Option<&[Record]>) -> Result<Response, Error> {
        let line: Vec<String> = fields.iter().map(|f| field(f, false)).collect();
        let records = match bulk {
            Some(b) => Some(
                b.iter()
                    .map(protocol_record)
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            None => None,
        };
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
                let line = self.write(&format!("set\t{}", protocol_record(one)?));
                self.call(&line, None).map(drop)
            }
            many => {
                let lines = many
                    .iter()
                    .map(protocol_record)
                    .collect::<Result<Vec<_>, _>>()?;
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
