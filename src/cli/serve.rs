//! `serve` and `stop` from the command line (§21.13).

use std::sync::Arc;

use crate::cli::{Ctx, NoSigpipe};
use crate::handler::server::{self, Config, InfoFn};
use crate::{Error, WarnFn, Warning};

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
        errors: Some(Arc::new(|w: &Warning| {
            diag!("tsvz: {}: {}", w.part.display(), w.message);
        }) as WarnFn),
        info,
        verbose: ctx.args.verbose,
    };
    // `serve` gives its signal mask back as it returns, and this command goes on for a moment
    // (the lost-write report, the exit): a signal in that moment would end it by its default
    // action after the stop was done. Blocked here, the three stay blocked to the exit, where
    // a pending one is dropped, as when `serve` never gave the mask back (0.2.1).
    // SAFETY: sigemptyset and sigaddset fill the set; pthread_sigmask changes this thread's mask.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            libc::sigaddset(&mut set, signal);
        }
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
    }
    server::serve(&ctx.store, config)?;
    Ok(0)
}

pub(crate) fn stop(ctx: &mut Ctx) -> Result<i32, Error> {
    let _sigpipe = NoSigpipe::new(); // a handler already stopping may have closed the connection
    server::stop(&ctx.store)?;
    Ok(0)
}
