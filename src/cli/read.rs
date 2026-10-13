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
