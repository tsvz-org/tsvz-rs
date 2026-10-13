//! Records and table output (§20.4).

use std::io::{self, Write};
use std::path::Path;

use crate::Error;
use crate::cli::{Ctx, table};
use crate::ops::{self, Out};

pub(crate) fn stdout_error(e: io::Error) -> Error {
    Error::io(Path::new("stdout"), e)
}

enum Layout {
    /// Rows: records as they come, or a table at the end.
    Rows,
    /// `verify` and `parts` lines: TAB-joined, or a table under this header (nothing at all
    /// when there are no lines).
    Fields(&'static [&'static str]),
    /// `len`: the count alone.
    Count,
}

/// Prints what an operation emits (`ops::Out`) in the chosen format.
pub(crate) struct Printer<'c> {
    ctx: &'c mut Ctx,
    layout: Layout,
    buf: String,
    table: Option<Vec<Vec<String>>>,
}

impl<'c> Printer<'c> {
    fn new(ctx: &'c mut Ctx, layout: Layout) -> Printer<'c> {
        let table = ctx.table.then(Vec::new);
        Printer {
            ctx,
            layout,
            buf: String::new(),
            table,
        }
    }

    pub(crate) fn rows(ctx: &'c mut Ctx) -> Printer<'c> {
        Printer::new(ctx, Layout::Rows)
    }

    pub(crate) fn fields(ctx: &'c mut Ctx, header: &'static [&'static str]) -> Printer<'c> {
        Printer::new(ctx, Layout::Fields(header))
    }

    pub(crate) fn count(ctx: &'c mut Ctx) -> Printer<'c> {
        Printer::new(ctx, Layout::Count)
    }

    fn write(&mut self) -> Result<(), Error> {
        let result = self.ctx.out.write_all(self.buf.as_bytes());
        self.buf.clear();
        result.map_err(stdout_error)
    }

    /// A table is followed by an empty line, as TSVZ.py prints it.
    pub(crate) fn finish(self) -> Result<(), Error> {
        let Some(rows) = self.table else {
            return Ok(());
        };
        let text = match self.layout {
            Layout::Rows => table::rows_table(&rows) + "\n",
            Layout::Fields(header) if !rows.is_empty() => {
                table::fields_table(header, &rows, table::terminal_columns()) + "\n"
            }
            _ => return Ok(()),
        };
        self.ctx
            .out
            .write_all(text.as_bytes())
            .map_err(stdout_error)
    }
}

impl Out for Printer<'_> {
    fn row<'a>(&mut self, key: &str, cells: impl Iterator<Item = &'a str>) -> Result<(), Error> {
        if let Some(rows) = &mut self.table {
            let mut row = vec![key.to_string()];
            row.extend(cells.map(str::to_string));
            rows.push(row);
            return Ok(());
        }
        let (d, strict) = (self.ctx.format.delimiter, self.ctx.format.strict);
        ops::record_line(&mut self.buf, key, cells, d, strict);
        self.write()
    }

    fn fields(&mut self, fields: &[&str]) -> Result<(), Error> {
        if let Some(rows) = &mut self.table {
            rows.push(fields.iter().map(|f| f.to_string()).collect());
            return Ok(());
        }
        ops::fields_line(&mut self.buf, fields, false);
        self.write()
    }

    fn count(&mut self, n: usize) -> Result<(), Error> {
        writeln!(self.ctx.out, "{n}").map_err(stdout_error)
    }
}
