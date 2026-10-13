//! Records output (§20.4.1). Task 14 adds the table layout.

use std::io::{self, Write};
use std::path::Path;

use crate::cli::{Ctx, table};
use crate::{Error, escape};

pub(crate) fn stdout_error(e: io::Error) -> Error {
    Error::io(Path::new("stdout"), e)
}

/// One row as a record: the key (`<#>` for a leading `#`), then its cells, joined by the
/// store's delimiter. Strict variants are §13-encoded; a loose file's cells are written as
/// they are (§20.4.4).
pub(crate) fn record<'a>(
    out: &mut impl Write,
    buf: &mut String,
    key: &str,
    cells: impl Iterator<Item = &'a str>,
    delimiter: u8,
    strict: bool,
) -> io::Result<()> {
    buf.clear();
    if strict {
        escape::encode_into(buf, key, delimiter, true);
    } else {
        buf.push_str(key);
    }
    for cell in cells {
        buf.push(delimiter as char);
        if strict {
            escape::encode_into(buf, cell, delimiter, false);
        } else {
            buf.push_str(cell);
        }
    }
    buf.push('\n');
    out.write_all(buf.as_bytes())
}

/// A `verify` or `parts` line: fields joined by TAB, each §13-encoded for TSVZ (§20.4.1).
pub(crate) fn fields(out: &mut impl Write, fields: &[&str]) -> io::Result<()> {
    let mut line = String::new();
    for (i, f) in fields.iter().enumerate() {
        if i > 0 {
            line.push('\t');
        }
        escape::encode_into(&mut line, f, b'\t', false);
    }
    line.push('\n');
    out.write_all(line.as_bytes())
}

/// Prints rows in the chosen format: records as they come, or a table at the end.
pub(crate) struct Emitter<'c> {
    ctx: &'c mut Ctx,
    buf: String,
    table: Option<Vec<Vec<String>>>,
}

impl<'c> Emitter<'c> {
    pub(crate) fn new(ctx: &'c mut Ctx) -> Emitter<'c> {
        let table = ctx.table.then(Vec::new);
        Emitter {
            ctx,
            buf: String::new(),
            table,
        }
    }

    pub(crate) fn row<'a>(
        &mut self,
        key: &str,
        cells: impl Iterator<Item = &'a str>,
    ) -> Result<(), Error> {
        if let Some(rows) = &mut self.table {
            let mut row = vec![key.to_string()];
            row.extend(cells.map(str::to_string));
            rows.push(row);
            return Ok(());
        }
        let (d, strict) = (self.ctx.format.delimiter, self.ctx.format.strict);
        record(&mut self.ctx.out, &mut self.buf, key, cells, d, strict).map_err(stdout_error)
    }

    /// A table is followed by an empty line, as TSVZ.py prints it.
    pub(crate) fn finish(self) -> Result<(), Error> {
        if let Some(rows) = self.table {
            let text = table::rows_table(&rows) + "\n";
            self.ctx
                .out
                .write_all(text.as_bytes())
                .map_err(stdout_error)?;
        }
        Ok(())
    }
}

/// Prints `verify` and `parts` lines: TAB-joined, or a table under `header` (nothing at all
/// when there are no lines).
pub(crate) struct Fields<'c> {
    ctx: &'c mut Ctx,
    header: &'static [&'static str],
    rows: Vec<Vec<String>>,
}

impl<'c> Fields<'c> {
    pub(crate) fn new(ctx: &'c mut Ctx, header: &'static [&'static str]) -> Fields<'c> {
        Fields {
            ctx,
            header,
            rows: Vec::new(),
        }
    }

    pub(crate) fn line(&mut self, values: &[&str]) -> Result<(), Error> {
        if self.ctx.table {
            self.rows
                .push(values.iter().map(|v| v.to_string()).collect());
            return Ok(());
        }
        fields(&mut self.ctx.out, values).map_err(stdout_error)
    }

    pub(crate) fn finish(self) -> Result<(), Error> {
        if self.ctx.table && !self.rows.is_empty() {
            let text =
                table::fields_table(self.header, &self.rows, table::terminal_columns()) + "\n";
            self.ctx
                .out
                .write_all(text.as_bytes())
                .map_err(stdout_error)?;
        }
        Ok(())
    }
}
