//! The command-line tool of §20.

pub mod args;
mod maint;
pub(crate) mod output;
mod read;
mod write;

use std::ffi::OsString;
use std::io::{BufWriter, StdoutLock, Write};
use std::path::PathBuf;
use std::sync::Arc;

use crate::name::{self, Format};
use crate::{Error, Options, Scope, Store, WarnFn, Warning};
use args::{Args, OutputFormat, Parsed};

pub const USAGE: &str = "\
usage: tsvz [OPTION ...] OPERATION STORE [ARG ...]
       tsvz [OPTION ...] STORE [OPERATION] [ARG ...]

Read and write tsvz-spec-v1 key-value stores (.tsvz .csvz .nsvz .psvz, and .tsv .csv .nsv .psv).

operations:
  read STORE                   print every live row
  get STORE KEY [KEY ...]      print the rows of KEYs (exit 3 if one is missing)
  set STORE KEY [VALUE ...]    write a row; KEY alone deletes it; '-' reads rows from stdin
  append ...                   same as set
  delete STORE KEY [KEY ...]   delete KEYs; '-' reads keys from stdin
  clear STORE                  empty the store, keeping its header and markers
  scrub STORE                  compact a one-part store in place
  verify STORE                 check #_checksum_<algo>_# segments (exit 4 on a mismatch)
  parts STORE                  list the parts of the store
  has STORE KEY                exit 0 if KEY is live, 3 if not
  len STORE                    print the number of live keys
  keys STORE                   print the live keys
  pop STORE KEY                print the row of KEY and delete it (exit 3 if missing)
  popitem STORE [first|last]   pop the first or last live key (default: last)
  setdefault STORE KEY VALUE [VALUE ...]
                               print the row of KEY, setting it to the VALUEs if missing
  serve STORE, stop STORE      not provided by this tsvz (exit 1)

options:
  -h, --help                   show this help
  -V, --version                show the version
  --format table|records       output format (default: table on a terminal, else records)
  -q, --quiet                  print errors only
  -v, --verbose                print informational messages
  -d, --delimiter D            delimiter of a .tsv-family file: tab, comma, pipe, null or one character
  --                           every argument after this is positional

exit status: 0 done, 1 failed or refused, 2 usage error, 3 missing key, 4 checksum mismatch
";

/// Run the tool with `argv` (without the program name) and return its exit status (§20.6).
pub fn main(argv: Vec<OsString>) -> i32 {
    // SAFETY: restores the default action for SIGPIPE before any other thread exists, so writing
    // to a closed pipe ends the process quietly, as it does other Unix tools.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let mut text = Vec::with_capacity(argv.len());
    for arg in argv {
        match arg.into_string() {
            Ok(s) => text.push(s),
            Err(bad) => {
                eprintln!("tsvz: an argument is not valid UTF-8: {}", bad.to_string_lossy());
                return 2;
            }
        }
    }
    match args::parse(&text) {
        Err(message) => {
            eprintln!("tsvz: {message}\nTry 'tsvz --help'.");
            2
        }
        Ok(Parsed::Help) => {
            print!("{USAGE}");
            0
        }
        Ok(Parsed::Version) => {
            println!("tsvz-rs {}", env!("CARGO_PKG_VERSION"));
            0
        }
        Ok(Parsed::Run(args)) => match Ctx::new(args) {
            Ok(mut ctx) => ctx.run(),
            Err(e) => {
                eprintln!("tsvz: {e}");
                1
            }
        },
    }
}

/// What every operation needs.
pub(crate) struct Ctx {
    pub args: Args,
    pub store: PathBuf,
    pub format: Format,
    #[allow(dead_code)] // read by the table output (Task 14), which removes this line
    pub table: bool,
    pub warn: WarnFn,
    pub out: BufWriter<StdoutLock<'static>>,
}

impl Ctx {
    fn new(args: Args) -> Result<Ctx, Error> {
        let store = PathBuf::from(&args.store);
        let mut format = name::format_of(&store).ok_or_else(|| Error::BadPath(store.clone()))?;
        let quiet = args.quiet;
        let warn: WarnFn = Arc::new(move |w: &Warning| {
            if !quiet {
                eprintln!("tsvz: {}: {}", w.part.display(), w.message);
            }
        });
        if let Some(d) = args.delimiter {
            if !format.strict {
                format.delimiter = d;
            } else if d != format.delimiter {
                warn(&Warning {
                    part: store.clone(),
                    offset: None,
                    message: "-d ignored: a strict store's extension decides its delimiter (§5.1)".into(),
                });
            }
        }
        // SAFETY: isatty only inspects file descriptor 1.
        let terminal = unsafe { libc::isatty(1) } == 1;
        let table = match args.format {
            Some(OutputFormat::Table) => true,
            Some(OutputFormat::Records) => false,
            None => terminal,
        };
        Ok(Ctx { args, store, format, table, warn, out: BufWriter::with_capacity(1 << 16, std::io::stdout().lock()) })
    }

    pub(crate) fn options(&self, scope: Scope, verify: bool) -> Options {
        Options { scope, verify, delimiter: self.args.delimiter, warnings: Some(self.warn.clone()) }
    }

    pub(crate) fn open(&self, scope: Scope, verify: bool) -> Result<Store, Error> {
        Store::open(&self.store, self.options(scope, verify))
    }

    /// An informational message, printed with -v (§20.7.1).
    pub(crate) fn info(&self, message: &str) {
        if self.args.verbose {
            eprintln!("tsvz: {}: {}", self.store.display(), message);
        }
    }

    fn run(&mut self) -> i32 {
        let result = match self.args.op.as_str() {
            "read" => read::read(self),
            "get" => read::get(self),
            "has" => read::has(self),
            "len" => read::len(self),
            "keys" => read::keys(self),
            "verify" => read::verify(self),
            "parts" => read::parts(self),
            "set" | "append" => write::set(self),
            "delete" => write::delete(self),
            "pop" => write::pop(self),
            "popitem" => write::popitem(self),
            "setdefault" => write::setdefault(self),
            "clear" => maint::run(self, true),
            "scrub" => maint::run(self, false),
            "serve" | "stop" => {
                eprintln!("tsvz: {}: this tsvz does not provide a handler (§21); work on the files instead", self.args.op);
                Ok(1)
            }
            other => {
                eprintln!("tsvz: unknown operation {other}");
                Ok(2)
            }
        };
        let code = result.unwrap_or_else(|e| {
            eprintln!("tsvz: {e}");
            1
        });
        if let Err(e) = self.out.flush() {
            if e.kind() != std::io::ErrorKind::BrokenPipe {
                eprintln!("tsvz: stdout: {e}");
                return 1;
            }
        }
        code
    }
}
