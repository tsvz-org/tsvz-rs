//! §20.1 argument parsing.

use std::collections::VecDeque;
use std::time::Duration;

/// The operations of §20.2, plus `serve` and `stop` (§20.2.6).
pub const OPERATIONS: &[&str] = &[
    "read",
    "get",
    "set",
    "append",
    "delete",
    "clear",
    "scrub",
    "verify",
    "parts",
    "has",
    "len",
    "keys",
    "pop",
    "popitem",
    "setdefault",
    "serve",
    "stop",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputFormat {
    Table,
    Records,
}

/// A parsed command line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Args {
    pub format: Option<OutputFormat>,
    pub quiet: bool,
    pub verbose: bool,
    pub delimiter: Option<u8>,
    /// `--x-direct`: work on the files even when a handler serves the store.
    pub direct: bool,
    /// `--x-idle-timeout S` (serve).
    pub idle_timeout: Option<Duration>,
    /// `--x-group G` (serve), as a gid.
    pub group: Option<u32>,
    /// `--x-mode M` (serve): the socket's mode.
    pub mode: Option<u32>,
    pub op: String,
    pub store: String,
    pub args: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Parsed {
    Help,
    Version,
    Run(Args),
}

/// (spelling, destination, takes a value).
const OPTIONS: &[(&str, &str, bool)] = &[
    ("-h", "help", false),
    ("--help", "help", false),
    ("-V", "version", false),
    ("--version", "version", false),
    ("--format", "format", true),
    ("-q", "quiet", false),
    ("--quiet", "quiet", false),
    ("-v", "verbose", false),
    ("--verbose", "verbose", false),
    ("-d", "delimiter", true),
    ("--delimiter", "delimiter", true),
    ("--x-direct", "direct", false),
    ("--x-idle-timeout", "idle", true),
    ("--x-group", "group", true),
    ("--x-mode", "mode", true),
];

/// An option's destination and whether it takes a value; a long option may be given by a
/// unique prefix, as TSVZ.py's parser accepts (`--delim`).
fn option(name: &str) -> Result<(&'static str, bool), String> {
    if let Some(&(_, dest, takes)) = OPTIONS.iter().find(|o| o.0 == name) {
        return Ok((dest, takes));
    }
    if name.starts_with("--") {
        let matches: Vec<_> = OPTIONS
            .iter()
            .filter(|o| o.0.starts_with("--") && o.0.starts_with(name))
            .collect();
        if let Some(first) = matches.first() {
            if matches.iter().all(|o| o.1 == first.1) {
                return Ok((first.1, first.2));
            }
            let names: Vec<&str> = matches.iter().map(|o| o.0).collect();
            return Err(format!("ambiguous option {name}: {}", names.join(", ")));
        }
    }
    Err(format!("unknown option {name}"))
}

/// `-` followed by a digit or `.` is a value, not an option (§20.1.2).
fn is_negative_number(arg: &str) -> bool {
    let mut chars = arg.chars();
    chars.next() == Some('-') && chars.next().is_some_and(|c| c.is_ascii_digit() || c == '.')
}

fn parse_idle(text: &str) -> Result<Duration, String> {
    text.parse::<f64>()
        .ok()
        .and_then(|s| Duration::try_from_secs_f64(s).ok())
        .ok_or_else(|| format!("--x-idle-timeout takes seconds, not {text:?}"))
}

fn parse_mode(text: &str) -> Result<u32, String> {
    u32::from_str_radix(text, 8)
        .ok()
        .filter(|&m| m <= 0o777)
        .ok_or_else(|| format!("--x-mode takes an octal mode such as 660, not {text:?}"))
}

/// A group name or number as a gid.
fn parse_group(text: &str) -> Result<u32, String> {
    if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) {
        return text
            .parse()
            .map_err(|_| format!("--x-group: bad group {text:?}"));
    }
    let name =
        std::ffi::CString::new(text).map_err(|_| format!("--x-group: bad group {text:?}"))?;
    // SAFETY: getgrnam reads the NUL-terminated name; the entry it returns is read at once,
    // before any other call could reuse it.
    let entry = unsafe { libc::getgrnam(name.as_ptr()) };
    if entry.is_null() {
        return Err(format!("--x-group: no group named {text:?}"));
    }
    // SAFETY: entry is a valid group entry (checked above).
    Ok(unsafe { (*entry).gr_gid })
}

fn parse_delimiter(text: &str) -> Result<u8, String> {
    match text {
        "tab" => Ok(b'\t'),
        "comma" => Ok(b','),
        "pipe" => Ok(b'|'),
        "null" => Ok(0),
        t if t.len() == 1 && t.is_ascii() && t != "\n" && t != "\r" => Ok(t.as_bytes()[0]),
        t => Err(format!(
            "-d takes one ASCII character or tab, comma, pipe or null, not {t:?}"
        )),
    }
}

/// Parse a command line per §20.1. Err is the message for a usage error (exit 2).
pub fn parse(argv: &[String]) -> Result<Parsed, String> {
    let mut a = Args::default();
    let (mut help, mut version) = (false, false);
    let (mut format, mut delimiter) = (None, None);
    let mut positionals = Vec::new();
    let mut queue: VecDeque<String> = argv.iter().cloned().collect();
    while let Some(arg) = queue.pop_front() {
        if arg == "--" {
            positionals.extend(queue.drain(..));
            break;
        }
        if !arg.starts_with('-') || arg == "-" || is_negative_number(&arg) {
            positionals.push(arg);
            continue;
        }
        let (name, value) = if let Some(long) = arg.strip_prefix("--") {
            match long.split_once('=') {
                Some((n, v)) => (format!("--{n}"), Some(v.to_string())),
                None => (arg.clone(), None),
            }
        } else {
            let split = arg.char_indices().nth(2).map_or(arg.len(), |(i, _)| i);
            let (flag, attached) = arg.split_at(split);
            if attached.is_empty() {
                (flag.to_string(), None)
            } else if option(flag).is_ok_and(|o| o.1) {
                (
                    flag.to_string(),
                    Some(attached.strip_prefix('=').unwrap_or(attached).to_string()),
                ) // -dcomma, -d=,
            } else {
                queue.push_front(format!("-{attached}")); // -qv: one flag at a time
                (flag.to_string(), None)
            }
        };
        let (dest, takes) = option(&name)?;
        let value = match (takes, value) {
            (true, Some(v)) => v,
            (true, None) => queue
                .pop_front()
                .ok_or_else(|| format!("option {name} needs a value"))?,
            (false, Some(_)) => return Err(format!("option {name} takes no value")),
            (false, None) => String::new(),
        };
        match dest {
            "help" => help = true,
            "version" => version = true,
            "format" => format = Some(value),
            "quiet" => a.quiet = true,
            "verbose" => a.verbose = true,
            "direct" => a.direct = true,
            // 0 means no timeout, as in TSVZ.py.
            "idle" => a.idle_timeout = Some(parse_idle(&value)?).filter(|d| !d.is_zero()),
            "group" => a.group = Some(parse_group(&value)?),
            "mode" => a.mode = Some(parse_mode(&value)?),
            _ => delimiter = Some(value),
        }
    }
    if help {
        return Ok(Parsed::Help);
    }
    if version {
        return Ok(Parsed::Version);
    }
    a.format = match format.as_deref() {
        None => None,
        Some("table") => Some(OutputFormat::Table),
        Some("records") => Some(OutputFormat::Records),
        Some(other) => return Err(format!("--format must be table or records, not {other:?}")),
    };
    a.delimiter = delimiter.as_deref().map(parse_delimiter).transpose()?;
    let mut pos = positionals.into_iter();
    let first = pos.next().ok_or("missing OPERATION and STORE")?;
    if OPERATIONS.contains(&first.as_str()) {
        a.store = pos.next().ok_or_else(|| format!("{first} needs a STORE"))?;
        a.op = first;
        a.args = pos.collect();
    } else {
        a.store = first;
        match pos.next() {
            None => a.op = "read".to_string(),
            Some(op) if OPERATIONS.contains(&op.as_str()) => {
                a.op = op;
                a.args = pos.collect();
            }
            Some(op) => {
                return Err(format!(
                    "unknown operation: neither {:?} nor {op:?} is one of {}",
                    a.store,
                    OPERATIONS.join(", ")
                ));
            }
        }
    }
    crate::ops::check_args(&a.op, &a.args)?;
    Ok(Parsed::Run(a))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Result<Parsed, String> {
        parse(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    fn run(args: &[&str]) -> Args {
        match p(args) {
            Ok(Parsed::Run(a)) => a,
            other => panic!("{args:?}: {other:?}"),
        }
    }

    #[test]
    fn canonical_and_file_first_forms() {
        let a = run(&["get", "s.tsvz", "k1", "k2"]);
        assert_eq!(
            (a.op.as_str(), a.store.as_str(), a.args.clone()),
            ("get", "s.tsvz", vec!["k1".to_string(), "k2".to_string()])
        );
        let b = run(&["s.tsvz", "append", "k", "v"]);
        assert_eq!((b.op.as_str(), b.store.as_str()), ("append", "s.tsvz"));
        let c = run(&["s.tsvz"]);
        assert_eq!(c.op, "read");
        assert!(
            p(&["s.tsvz", "alice"])
                .unwrap_err()
                .contains("unknown operation")
        );
        assert!(p(&["./read", "keys"]).is_ok());
    }

    #[test]
    fn options_anywhere_before_double_dash() {
        let a = run(&["set", "-q", "s.tsvz", "k", "--format", "records", "v", "-v"]);
        assert!(a.quiet && a.verbose);
        assert_eq!(a.format, Some(OutputFormat::Records));
        assert_eq!(a.args, vec!["k", "v"]);
        let b = run(&["set", "s.tsvz", "--", "carol", "-q", "--x"]);
        assert_eq!(b.args, vec!["carol", "-q", "--x"]);
        assert!(!b.quiet);
    }

    #[test]
    fn dashes_and_negative_numbers_are_values() {
        assert_eq!(run(&["set", "s.tsvz", "-"]).args, vec!["-"]);
        assert_eq!(
            run(&["set", "s.tsvz", "k", "-5", "-.5", "-1x"]).args,
            vec!["k", "-5", "-.5", "-1x"]
        );
    }

    #[test]
    fn grouped_flags_attached_values_and_prefixes() {
        let a = run(&["-qv", "read", "s.tsv", "-dcomma"]);
        assert!(a.quiet && a.verbose);
        assert_eq!(a.delimiter, Some(b','));
        assert_eq!(run(&["read", "s.tsv", "-d=|"]).delimiter, Some(b'|'));
        assert_eq!(
            run(&["read", "s.tsv", "--delimiter=null"]).delimiter,
            Some(0)
        );
        assert_eq!(
            run(&["read", "s.tsv", "--delim", "tab"]).delimiter,
            Some(b'\t')
        );
        assert_eq!(
            run(&["--form=table", "read", "s.tsvz"]).format,
            Some(OutputFormat::Table)
        );
        assert!(
            p(&["--ver", "read", "s.tsvz"])
                .unwrap_err()
                .contains("ambiguous")
        );
    }

    #[test]
    fn usage_errors() {
        for (args, want) in [
            (&["read"][..], "needs a STORE"),
            (&[][..], "missing OPERATION"),
            (&["--bogus", "read", "s.tsvz"][..], "unknown option"),
            (&["-x", "read", "s.tsvz"][..], "unknown option"),
            (&["-ü", "read", "s.tsvz"][..], "unknown option"),
            (&["--format", "json", "read", "s.tsvz"][..], "--format"),
            (&["--format"][..], "needs a value"),
            (&["--quiet=yes", "read", "s.tsvz"][..], "takes no value"),
            (&["-d", "ab", "read", "s.tsv"][..], "-d takes"),
            (&["read", "s.tsvz", "extra"][..], "takes no arguments"),
            (&["popitem", "s.tsvz", "middle"][..], "first or last"),
            (&["has", "s.tsvz", "a", "b"][..], "one KEY"),
            (&["setdefault", "s.tsvz", "k"][..], "a KEY and a VALUE"),
            (&["get", "s.tsvz"][..], "needs a KEY"),
            (&["get", "s.tsvz", "a", ""][..], "must not be empty"),
            (&["set", "s.tsvz", "", "v"][..], "must not be empty"),
        ] {
            let err = p(args).unwrap_err();
            assert!(err.contains(want), "{args:?}: {err}");
        }
    }

    #[test]
    fn an_idle_timeout_of_zero_means_none() {
        // As in TSVZ.py: 0 (or nothing) keeps the handler serving.
        assert_eq!(
            run(&["serve", "s.tsvz", "--x-idle-timeout", "0"]).idle_timeout,
            None
        );
        assert_eq!(
            run(&["serve", "s.tsvz", "--x-idle-timeout", "1.5"]).idle_timeout,
            Some(Duration::from_millis(1500))
        );
        assert!(p(&["serve", "s.tsvz", "--x-idle-timeout", "-1"]).is_err());
    }

    #[test]
    fn help_and_version_win() {
        assert_eq!(p(&["--help", "bogus"]).unwrap(), Parsed::Help);
        assert_eq!(p(&["read", "s.tsvz", "-V"]).unwrap(), Parsed::Version);
    }
}
