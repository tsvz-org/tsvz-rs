//! Request and response lines of the handler protocol (§21.4–§21.6, §21.10).

use std::io::{self, BufRead, Read};

use crate::escape::{self, is_marker_key};
use crate::ops::check_args;

/// A line longer than this closes the connection (§21.10.2 asks for at least 1 MiB; TSVZ.py's
/// limit).
pub const LINE_LIMIT: usize = 64 << 20;

/// The operations a handler answers: those of §20.2 except `serve`, and `stop` (§21.7.1).
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
    "stop",
];

/// A parsed request (§21.5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// `--sync`: acknowledge writes once they are on durable storage (§21.5.2).
    pub sync: bool,
    /// `--x-written` (TSVZ.py's extension): acknowledge writes once written, and answer status 1
    /// when the write failed.
    pub written: bool,
    pub op: String,
    /// The arguments, §13-decoded.
    pub args: Vec<String>,
    /// The arguments as written: a marker key is told from a data key by how it is written
    /// (§21.4.3).
    pub raw: Vec<String>,
}

impl Request {
    /// Whether record lines up to a `#` line follow this request (§21.7.2).
    pub fn bulk(&self) -> bool {
        matches!(self.op.as_str(), "set" | "append" | "delete") && self.raw == ["-"]
    }
}

/// Parse a request line, without its line end (§21.5). The error is the message of a usage
/// error: status 2, and the connection stays usable (§21.10.1).
pub fn parse(line: &str) -> Result<Request, String> {
    let fields: Vec<&str> = line.split('\t').collect();
    let (mut sync, mut written) = (false, false);
    let mut i = 0;
    while i < fields.len() && fields[i].starts_with("--") {
        match fields[i] {
            "--sync" => sync = true,
            "--x-written" => written = true,
            other => return Err(format!("unknown option {other}")),
        }
        i += 1;
    }
    let op = fields.get(i).copied().unwrap_or("");
    if op.is_empty() {
        return Err("missing operation".into());
    }
    if !OPERATIONS.contains(&op) {
        return Err(format!("unknown operation {op}"));
    }
    let raw: Vec<String> = fields[i + 1..].iter().map(|f| f.to_string()).collect();
    let args: Vec<String> = raw.iter().map(|f| decode(f)).collect();
    check_args(op, &args)?;
    if op == "setdefault" && is_marker_key(&raw[0]) {
        return Err("setdefault takes a data KEY, not a marker key (§21.7.3)".into());
    }
    Ok(Request {
        sync,
        written,
        op: op.to_string(),
        args,
        raw,
    })
}

/// Whether record lines up to a `#` line follow the request line `line` (§21.7.2), judged by its
/// shape alone, as TSVZ.py's `_serveNeedsBulk` judges it: `set -`, `append -` or `delete -`
/// after any options, known or not. A request refused for its options still has its records
/// read, so that they are never taken for requests.
pub fn needs_bulk(line: &str) -> bool {
    let mut fields = line.split('\t').skip_while(|f| f.starts_with("--"));
    matches!(fields.next(), Some("set" | "append" | "delete"))
        && fields.next() == Some("-")
        && fields.next().is_none()
}

/// A field of the protocol: §13 with TAB whatever the store's variant (§21.4.2); with `key`, a
/// leading `#` is written `<#>`.
pub fn field(value: &str, key: bool) -> String {
    escape::encode(value, b'\t', key)
}

/// A protocol field, decoded (§21.4.3).
pub fn decode(field: &str) -> String {
    escape::decode(field, b'\t').into_owned()
}

/// The decoded fields of an output or record line.
pub fn decode_fields(line: &str) -> Vec<String> {
    line.split('\t').map(decode).collect()
}

/// A response (§21.6).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Response {
    /// The exit status of §20.6.
    pub status: i32,
    /// The output lines as sent: rows as `read --format records` prints them in the TSVZ
    /// variant, keys, the count of `len`, `verify` and `parts` fields (§21.6.1).
    pub lines: Vec<String>,
    /// The diagnostics (`#!` lines), decoded.
    pub diagnostics: Vec<String>,
    /// The status line's message, decoded; empty when it has none.
    pub message: String,
}

impl Response {
    /// A response with no output and no message.
    pub fn status(status: i32) -> Response {
        Response {
            status,
            ..Response::default()
        }
    }

    /// A response with no output and a message.
    pub fn failed(status: i32, message: impl Into<String>) -> Response {
        Response {
            status,
            message: message.into(),
            ..Response::default()
        }
    }

    /// The response as sent: diagnostic lines, output lines, then the status line.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = String::new();
        for text in &self.diagnostics {
            out.push_str("#!\t");
            escape::encode_into(&mut out, text, b'\t', false);
            out.push('\n');
        }
        for line in &self.lines {
            out.push_str(line);
            out.push('\n');
        }
        out.push('#');
        out.push_str(&self.status.to_string());
        if !self.message.is_empty() {
            out.push('\t');
            escape::encode_into(&mut out, &self.message, b'\t', false);
        }
        out.push('\n');
        out.into_bytes()
    }
}

/// One line without its `\n` and one `\r` before it, invalid UTF-8 replaced (§21.4.1). None
/// at the end of the input, including a last line with no `\n`: it is not a whole line. A line
/// longer than `limit` bytes is an error (`InvalidData`).
pub fn read_line(input: &mut impl BufRead, limit: usize) -> io::Result<Option<String>> {
    let mut buf = Vec::new();
    input
        .by_ref()
        .take(limit as u64 + 2)
        .read_until(b'\n', &mut buf)?;
    if buf.last() != Some(&b'\n') {
        if buf.len() > limit {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "line too long"));
        }
        return Ok(None);
    }
    buf.pop();
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    if buf.len() > limit {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "line too long"));
    }
    Ok(Some(match String::from_utf8(buf) {
        Ok(text) => text,
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    }))
}

/// Read one response: lines up to its status line (§21.6). The connection ending first is
/// `UnexpectedEof`; a status that is not a number reads as 1.
pub fn read_response(input: &mut impl BufRead) -> io::Result<Response> {
    let mut response = Response::default();
    loop {
        let Some(line) = read_line(input, LINE_LIMIT)? else {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the handler closed the connection before its answer ended",
            ));
        };
        if let Some(text) = line.strip_prefix("#!") {
            response
                .diagnostics
                .push(decode(text.strip_prefix('\t').unwrap_or(text)));
        } else if let Some(status) = line.strip_prefix('#') {
            let (number, message) = status.split_once('\t').unwrap_or((status, ""));
            let digits = !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit());
            response.status = if digits {
                number.parse().unwrap_or(1)
            } else {
                1
            };
            response.message = decode(message);
            return Ok(response);
        } else {
            response.lines.push(line);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn requests_have_options_an_operation_and_decoded_arguments() {
        let r = parse("--sync\t--x-written\tset\t<#>k\ta<sep>b\t").unwrap();
        assert!(r.sync && r.written);
        assert_eq!(r.op, "set");
        assert_eq!(r.args, strings(&["#k", "a\tb", ""]));
        assert_eq!(r.raw, strings(&["<#>k", "a<sep>b", ""]));
        assert!(!r.bulk());
        let m = parse("set\t#_defaults_#\td1").unwrap();
        assert!(!m.sync && !m.written);
        assert_eq!(m.raw[0], "#_defaults_#");
        assert!(parse("set\t-").unwrap().bulk());
        assert!(parse("delete\t-").unwrap().bulk());
        assert!(!parse("get\t-").unwrap().bulk());
        assert!(parse("stop").is_ok() && parse("popitem\tfirst").is_ok());
    }

    #[test]
    fn bulk_is_judged_by_the_shape_of_the_line() {
        for line in [
            "set\t-",
            "--sync\tdelete\t-",
            "--x-future\tappend\t-",
            "--a\t--b\tset\t-",
        ] {
            assert!(needs_bulk(line), "{line:?}");
        }
        for line in ["set\tk", "set\t-\tv", "get\t-", "--sync", "", "set"] {
            assert!(!needs_bulk(line), "{line:?}");
        }
    }

    #[test]
    fn bad_requests_are_usage_errors() {
        for (line, want) in [
            ("", "missing operation"),
            ("--sync", "missing operation"),
            ("--bogus\tread", "unknown option --bogus"),
            ("serve", "unknown operation serve"),
            ("frobnicate\tk", "unknown operation"),
            ("get", "needs a KEY"),
            ("has", "needs a KEY"),
            ("get\ta\t", "must not be empty"),
            ("len\tx", "takes no arguments"),
            ("popitem\tmiddle", "first or last"),
            ("setdefault\t#_defaults_#\tx", "marker"),
        ] {
            let err = parse(line).unwrap_err();
            assert!(err.contains(want), "{line:?}: {err}");
        }
        // A data key written <#>… is not a marker key (§21.4.3).
        assert!(parse("setdefault\t<#>_defaults_#\tx").is_ok());
    }

    #[test]
    fn responses_encode_diagnostics_output_and_a_status_line() {
        let r = Response {
            status: 3,
            lines: strings(&["<#>k\tv", "2"]),
            diagnostics: strings(&["a\tb", "c\nd"]),
            message: "why\tnot".into(),
        };
        assert_eq!(
            String::from_utf8(r.encode()).unwrap(),
            "#!\ta<sep>b\n#!\tc<LF>d\n<#>k\tv\n2\n#3\twhy<sep>not\n"
        );
        assert_eq!(Response::status(0).encode(), b"#0\n");
        assert_eq!(Response::failed(2, "no").encode(), b"#2\tno\n");
    }

    #[test]
    fn responses_read_back_as_written() {
        let r = Response {
            status: 1,
            lines: strings(&["a\tx", "<#>h"]),
            diagnostics: strings(&["warn\tme"]),
            message: "could not read every part".into(),
        };
        let mut input = Cursor::new(r.encode());
        assert_eq!(read_response(&mut input).unwrap(), r);
        let mut cut = Cursor::new(b"a\tx\n".to_vec());
        assert_eq!(
            read_response(&mut cut).unwrap_err().kind(),
            std::io::ErrorKind::UnexpectedEof
        );
        let mut odd = Cursor::new(b"#x\n".to_vec());
        assert_eq!(read_response(&mut odd).unwrap().status, 1);
    }

    #[test]
    fn lines_end_at_newline_drop_one_cr_and_respect_the_limit() {
        let mut input = Cursor::new(b"a\r\r\nb\r\nc".to_vec());
        assert_eq!(read_line(&mut input, 10).unwrap().as_deref(), Some("a\r"));
        assert_eq!(read_line(&mut input, 10).unwrap().as_deref(), Some("b"));
        assert_eq!(
            read_line(&mut input, 10).unwrap(),
            None,
            "no \\n: not a line"
        );
        let mut long = Cursor::new(b"0123456789AB\n".to_vec());
        assert_eq!(
            read_line(&mut long, 10).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
        let mut exact = Cursor::new(b"0123456789\n".to_vec());
        assert_eq!(read_line(&mut exact, 10).unwrap().unwrap().len(), 10);
        let mut bad = Cursor::new(b"\xff\n".to_vec());
        assert_eq!(
            read_line(&mut bad, 10).unwrap().as_deref(),
            Some("\u{fffd}")
        );
    }

    #[test]
    fn fields_use_section_13_with_tab() {
        assert_eq!(field("#k\t", true), "<#>k<sep>");
        assert_eq!(field("#k", false), "#k");
        assert_eq!(decode_fields("<#>k\ta<sep>b"), strings(&["#k", "a\tb"]));
    }
}
