//! The reading operations: read, get, has, len, keys, verify, parts (§20.2).

use std::io::Write;

use crate::cli::output::{fields, stdout_error, Emitter};
use crate::cli::Ctx;
use crate::{parts, Error, Scope, Store};

/// A read that stopped at damaged data fails (§4.10, §20.6).
fn status(store: &Store, code: i32) -> i32 {
    if store.stopped().is_some() { 1 } else { code }
}

pub(crate) fn read(ctx: &mut Ctx) -> Result<i32, Error> {
    let store = ctx.open(Scope::All, false)?;
    let mut out = Emitter::new(ctx);
    for (key, row) in store.iter() {
        out.row(key, row.cells())?;
    }
    out.finish()?;
    ctx.info(&format!("{} live keys in {} parts", store.len(), store.parts().len()));
    Ok(status(&store, 0))
}

pub(crate) fn get(ctx: &mut Ctx) -> Result<i32, Error> {
    let keys = ctx.args.args.clone();
    let store = ctx.open(Scope::Keys(keys.clone()), false)?;
    let mut missing = false;
    let mut out = Emitter::new(ctx);
    for requested in &keys {
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
    out.finish()?;
    Ok(status(&store, if missing { 3 } else { 0 }))
}

pub(crate) fn has(ctx: &mut Ctx) -> Result<i32, Error> {
    let store = ctx.open(Scope::KeysOnly, false)?;
    let live = store.get(store.lookup_key(&ctx.args.args[0])).is_some();
    Ok(status(&store, if live { 0 } else { 3 }))
}

pub(crate) fn len(ctx: &mut Ctx) -> Result<i32, Error> {
    let store = ctx.open(Scope::KeysOnly, false)?;
    writeln!(ctx.out, "{}", store.len()).map_err(stdout_error)?;
    Ok(status(&store, 0))
}

pub(crate) fn keys(ctx: &mut Ctx) -> Result<i32, Error> {
    let store = ctx.open(Scope::KeysOnly, false)?;
    let mut out = Emitter::new(ctx);
    for (key, _) in store.iter() {
        out.row(key, std::iter::empty())?;
    }
    out.finish()?;
    Ok(status(&store, 0))
}

pub(crate) fn verify(ctx: &mut Ctx) -> Result<i32, Error> {
    let store = ctx.open(Scope::KeysOnly, true)?;
    for m in store.mismatches() {
        let part = m.part.display().to_string();
        fields(&mut ctx.out, &[&part, &m.line.to_string(), m.algo, &m.expected, &m.computed]).map_err(stdout_error)?;
    }
    Ok(status(&store, if store.mismatches().is_empty() { 0 } else { 4 }))
}

pub(crate) fn parts(ctx: &mut Ctx) -> Result<i32, Error> {
    let list = parts::list(&ctx.store)?;
    if list.is_empty() {
        return Err(Error::Missing(ctx.store.clone()));
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
        fields(&mut ctx.out, &[&i.to_string(), part.ordinal.as_deref().unwrap_or(""), &path, &flags.join(",")]).map_err(stdout_error)?;
    }
    Ok(0)
}
