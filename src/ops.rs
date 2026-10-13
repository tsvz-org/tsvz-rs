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
