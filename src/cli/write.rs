//! The writing operations: set, append, delete (with bulk input), pop, popitem, setdefault
//! (§20.2, §20.3).

use std::io::{Read, Write};
use std::path::Path;

use crate::cli::Ctx;
use crate::cli::output::{Emitter, stdout_error};
use crate::escape::{self, is_marker_key};
use crate::frame::Framer;
use crate::replay::{Cells, Markers};
use crate::store::Row;
use crate::write::{Record, Writer, tombstones};
use crate::{Error, Scope, Store, Warning};

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
fn bulk(ctx: &Ctx, keys_only: bool) -> Result<Vec<Record>, Error> {
    let mut input = Vec::new();
    std::io::stdin()
        .lock()
        .read_to_end(&mut input)
        .map_err(|e| Error::io(Path::new("standard input"), e))?;
    let format = ctx.format;
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
    framer.push(&input, &mut |line| {
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
        (ctx.warn)(&Warning {
            part: "standard input".into(),
            offset: None,
            message: "ignored the unterminated last line".into(),
        });
    }
    Ok(records)
}

/// Print one row and flush it, so that a row which cannot be printed is never deleted.
fn print(ctx: &mut Ctx, key: &str, cells: &[String]) -> Result<(), Error> {
    let mut out = Emitter::new(ctx);
    out.row(key, cells.iter().map(String::as_str))?;
    out.finish()?;
    ctx.out.flush().map_err(stdout_error)
}

/// `pop` (§20.2): print KEY's row and append its tombstone; 3 when it is missing.
pub(crate) fn pop(ctx: &mut Ctx) -> Result<i32, Error> {
    let requested = ctx.args.args[0].clone();
    let store = ctx.open(Scope::Keys(vec![requested.clone()]), false)?;
    if store.incomplete() {
        return Ok(1); // no change is based on a read that failed (§4.10)
    }
    let key = store.lookup_key(&requested).to_string();
    let Some(row) = store.get(&key) else {
        return Ok(3);
    };
    let cells = row.to_vec();
    let strip = ctx.format.strict && store.markers().strip;
    print(ctx, &key, &cells)?;
    let records = tombstones([key.as_str()], strip, ctx.format.delimiter);
    writer(ctx, Some(store.markers().member_size))?.append(&records)?;
    Ok(0)
}

/// `popitem [first|last]` (§20.2).
pub(crate) fn popitem(ctx: &mut Ctx) -> Result<i32, Error> {
    let store = ctx.open(Scope::All, false)?;
    if store.incomplete() {
        return Ok(1);
    }
    let pick = if ctx.args.args.first().map(String::as_str) == Some("first") {
        store.first()
    } else {
        store.last()
    };
    let Some((key, row)) = pick else { return Ok(3) };
    let (key, cells) = (key.to_string(), row.to_vec());
    let strip = ctx.format.strict && store.markers().strip;
    print(ctx, &key, &cells)?;
    let records = tombstones([key.as_str()], strip, ctx.format.delimiter);
    writer(ctx, Some(store.markers().member_size))?.append(&records)?;
    Ok(0)
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
    if store.as_ref().is_some_and(Store::incomplete) {
        return Ok(1);
    }
    let markers = store
        .as_ref()
        .map(|s| s.markers().clone())
        .unwrap_or_else(Markers::default);
    let strip = ctx.format.strict && markers.strip;
    let trim = |s: &str| {
        if strip {
            s.trim_end_matches([' ', '\t']).to_string()
        } else {
            s.to_string()
        }
    };
    let key = trim(&requested);
    if let Some(row) = store.as_ref().and_then(|s| s.get(&key)) {
        let cells = row.to_vec();
        print(ctx, &key, &cells)?;
        return Ok(0);
    }
    writer(ctx, Some(markers.member_size))?.append(&[Record::Data {
        key: requested.clone(),
        values: values.clone(),
    }])?;
    let written = Cells::from_values(values.iter().map(|v| trim(v)));
    // The row a reader then returns: padded to the store's width, or, in a store with no data
    // row yet, to the width this row sets (its own cells or the defaults, which Row covers).
    let width = store.as_ref().map_or(0, |s| s.width());
    let cells = Row::new(&written, &markers.defaults, markers.fill_empty, width).to_vec();
    print(ctx, &key, &cells)?;
    Ok(0)
}
