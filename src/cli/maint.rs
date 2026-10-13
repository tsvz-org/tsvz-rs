//! `clear` and `scrub` from the command line (§20.2).

use crate::cli::Ctx;
use crate::maint::{self, Outcome};
use crate::{Error, Scope};

pub(crate) fn run(ctx: &mut Ctx, clear: bool) -> Result<i32, Error> {
    let options = ctx.options(Scope::All, false);
    let outcome = if clear {
        maint::clear(&ctx.store, &options)?
    } else {
        maint::scrub(&ctx.store, &options)?
    };
    match outcome {
        Outcome::Written => ctx.info("rewritten"),
        Outcome::Unchanged => ctx.info("unchanged: nothing to write"),
        Outcome::Partial { backup } => {
            let kept = backup
                .map(|b| format!("; its original bytes are in {}", b.display()))
                .unwrap_or_default();
            eprintln!(
                "tsvz: {}: the read stopped at damaged data; rewrote what was read{kept}",
                ctx.store.display()
            );
            return Ok(1);
        }
    }
    Ok(0)
}
