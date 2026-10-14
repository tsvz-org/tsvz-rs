//! Forwarding a command line to the store's handler (§20.8).

use std::io::{BufWriter, Write};

use crate::cli::Ctx;
use crate::cli::output::{Printer, stdout_error};
use crate::cli::write::bulk;
use crate::handler::protocol::{Response, decode, decode_fields, field};
use crate::handler::{Client, pointer};
use crate::ops::{Out, protocol_record};
use crate::{Error, Warning};

/// What a command line forwards. `verify` and `parts` read the files themselves and gain nothing
/// from a handler (TSVZ.py routes them the same way); `serve` and `stop` are not forwarded.
const ROUTED: &[&str] = &[
    "read",
    "get",
    "set",
    "append",
    "delete",
    "clear",
    "scrub",
    "has",
    "len",
    "keys",
    "pop",
    "popitem",
    "setdefault",
];
/// Of those, the ones redone on the files when the handler goes away during the request.
const READ_ONLY: &[&str] = &["read", "get", "has", "len", "keys"];

fn warn(ctx: &Ctx, message: String) {
    (ctx.warn)(&Warning {
        part: ctx.store.clone(),
        offset: None,
        message,
    });
}

/// Run the command line through the handler of a strict store when one can be used (§20.8.1):
/// Some(exit status), or None to work on the files (§20.8.2).
pub(crate) fn route(ctx: &mut Ctx) -> Option<Result<i32, Error>> {
    let op = ctx.args.op.clone();
    if ctx.args.direct || !ctx.format.strict || !ROUTED.contains(&op.as_str()) {
        return None;
    }
    let info = match pointer::lookup(&ctx.store) {
        Ok(Some(info)) => info,
        Ok(None) => return None,
        Err(why) => {
            warn(
                ctx,
                format!("ignoring the pointer file: {why}; using the files directly"),
            );
            return None;
        }
    };
    let stream = match pointer::connect(&info, None) {
        Ok(stream) => stream,
        Err(e) => {
            if !e.quiet {
                let at = pointer::path_of(&ctx.store);
                let why = e.message;
                warn(
                    ctx,
                    format!(
                        "the handler in {} does not answer ({why}); using the files directly",
                        at.display()
                    ),
                );
            }
            return None;
        }
    };
    ctx.info(&format!(
        "via the handler at {} (pid {})",
        info.address, info.pid
    ));
    let mut client = Client::over(stream, &ctx.store).ok()?;
    let writes = matches!(op.as_str(), "set" | "append" | "delete");
    // A write is answered once written, so that one that fails exits 1 as on the files.
    let mut line = String::from(if writes { "--x-written\t" } else { "" });
    line.push_str(&op);
    for arg in &ctx.args.args {
        line.push('\t');
        line.push_str(&field(arg, false)); // the arguments are literal (§20.2)
    }
    let records = if writes && ctx.args.args == ["-"] {
        match bulk(ctx, op == "delete") {
            Ok(records) => Some(records.iter().map(protocol_record).collect::<Vec<_>>()),
            Err(e) => return Some(Err(e)),
        }
    } else {
        None
    };
    match client.send(&line, records.as_deref()) {
        Ok(response)
            if matches!(op.as_str(), "pop" | "popitem")
                && response.status == 0
                && !response.lines.is_empty() =>
        {
            Some(deliver(ctx, &mut client, &response))
        }
        Ok(response) => Some(print(ctx, &response)),
        Err(e) if READ_ONLY.contains(&op.as_str()) => {
            warn(ctx, format!("{e}; using the files directly"));
            None
        }
        Err(e) => Some(Err(e)),
    }
}

/// SIGPIPE ignored while this lives, so that writing to a closed pipe fails instead of ending
/// the process; its default action is restored after.
struct NoSigpipe;

impl NoSigpipe {
    fn new() -> NoSigpipe {
        // SAFETY: the command line runs one thread; this only changes SIGPIPE's action.
        unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN) };
        NoSigpipe
    }
}

impl Drop for NoSigpipe {
    fn drop(&mut self) {
        // SAFETY: as above.
        unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
    }
}

/// Print the row a `pop` or `popitem` took. The handler deleted it before answering (on the
/// files the row is printed first): when it cannot be printed (a closed pipe, a full disk), it
/// is put back with `setdefault`, which keeps a row written for its key since, and the command
/// fails.
fn deliver(ctx: &mut Ctx, client: &mut Client, response: &Response) -> Result<i32, Error> {
    let printed = {
        let _sigpipe = NoSigpipe::new();
        let printed = print(ctx, response)
            .and_then(|status| ctx.out.flush().map_err(stdout_error).map(|()| status));
        if printed.is_err() {
            // What stdout did not take is dropped, not written again at the end.
            let unwritten =
                std::mem::replace(&mut ctx.out, BufWriter::new(std::io::stdout().lock()));
            let _ = unwritten.into_parts();
        }
        printed
    };
    let Err(e) = printed else {
        return printed;
    };
    let fields = decode_fields(&response.lines[0]);
    let mut line = String::from("setdefault");
    for f in &fields {
        line.push('\t');
        line.push_str(&field(f, false));
    }
    let row = fields.join("\t");
    let store = ctx.store.display();
    match client.send(&line, None) {
        Ok(r) if r.status == 0 && r.lines.first() == Some(&response.lines[0]) => {
            diag!("tsvz: {store}: the row taken could not be printed; it was put back");
        }
        Ok(r) if r.status == 0 => diag!(
            "tsvz: {store}: the row taken could not be printed, and its key was written since: {row}"
        ),
        Ok(r) => diag!(
            "tsvz: {store}: the row taken could not be printed nor put back ({}): {row}",
            r.message
        ),
        Err(put) => {
            diag!("tsvz: {store}: the row taken could not be printed nor put back ({put}): {row}")
        }
    }
    Err(e)
}

/// Print a response as the operation prints on the files (§20.8.1).
fn print(ctx: &mut Ctx, response: &Response) -> Result<i32, Error> {
    if !ctx.args.quiet {
        for text in &response.diagnostics {
            diag!("tsvz: {text}");
        }
    }
    match ctx.args.op.as_str() {
        "read" | "get" | "pop" | "popitem" | "setdefault" => {
            let mut out = Printer::rows(ctx);
            for line in &response.lines {
                let fields = decode_fields(line);
                out.row(&fields[0], fields[1..].iter().map(String::as_str))?;
            }
            out.finish()?;
        }
        "keys" => {
            let mut out = Printer::rows(ctx);
            for line in &response.lines {
                out.row(&decode(line), std::iter::empty())?;
            }
            out.finish()?;
        }
        "len" => {
            for line in &response.lines {
                writeln!(ctx.out, "{line}").map_err(stdout_error)?;
            }
        }
        _ => {}
    }
    if matches!(response.status, 1 | 2) && !response.message.is_empty() {
        diag!("tsvz: {}: {}", ctx.store.display(), response.message);
    }
    Ok(response.status)
}
