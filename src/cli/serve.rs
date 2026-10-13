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
