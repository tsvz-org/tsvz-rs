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
/// A key that a reader would strip names a key of the store (§20.2.7), so the store is read
/// first; when it cannot be read whole, nothing is written and the exit status is 1.
pub(crate) fn delete(ctx: &mut Ctx) -> Result<i32, Error> {
    let mut records = if ctx.args.args == ["-"] {
        bulk(ctx, true)?
    } else {
        ctx.args
            .args
            .iter()
            .map(|k| Record::from_args(k, &[]))
            .collect()
    };
    let d = ctx.format.delimiter;
    if ctx.format.strict && ops::delete_needs_store(&records, d) {
        records = match ctx.open(Scope::KeysOnly, false) {
            Ok(store) if store.incomplete() => {
                diag!(
                    "tsvz: {}: could not read every part of the store; nothing was written",
                    ctx.store.display()
                );
                return Ok(1);
            }
            Ok(store) => {
                let strip = store.markers().strip;
                ops::delete_records(&records, d, strip, |k| store.get(k).is_some())
            }
            // A store delete creates starts with stripping on (§10.3).
            Err(Error::Missing(_)) => ops::delete_records(&records, d, true, |_| false),
            Err(e) => return Err(e),
        };
    }
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
