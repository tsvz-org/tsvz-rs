# Where tsvz-spec-v1 is silent

tsvz-rs follows `tsvz-spec-v1.md` wherever it decides. Where it leaves a choice open, tsvz-rs
does what TSVZ.py does, so the two tools agree in practice. Each choice is listed here with the
TSVZ.py code it copies. The differential harness (`difftest/`) finds new ones.

| Choice | tsvz-rs (as TSVZ.py) | TSVZ.py reference |
|---|---|---|
| `parts` ordinal | Printed as written in the file name (`002`, `B`), not normalized | `_partRows` |
| xz parts | A member may be an `.xz` stream or a legacy `.lzma` stream (auto-detected) | `lzma.LZMADecompressor()` |
| zstd skippable frames | Decoded as frames with no output | `ZstdDecompressor` |
| Appending after a torn tail | The uncommitted bytes after the last newline are cut first, with a warning, so the new record cannot join them | `_lockedAppend(…, 'truncate')` |
| `scrub` markers | `#_version_#` written as `1`; then strip, fill-empty, return-defaults, rotate, write-ack, member-size and on-corruption when not default; `#_defaults_#` last | `_specPreamble` |
| `scrub` rows | Fully resolved, padded with empty cells to the final defaults' width; the §19.4 compensating layout only when a row would not read back the same | `_specScrubCells`, `_specRoundTrips` |
| `scrub` and checksum markers | Not kept: a snapshot has no segments to verify | `_specScrub` |
| `clear` of one part | Header comment and markers, without `#_version_#` | `_specClearTabularFile` |
| A damaged part before a rewrite | Copied to `<part>.damaged-<YYYYmmddTHHMMSS>` (`.1`, `.2`, … when taken) and synced | `_specBackupPart` |
| The backup copy's permissions | Not as TSVZ.py (which uses the default mode): the part's read bits plus owner write (`mode & 0o644`, less the umask), so a private store's copy stays private and never carries setuid, setgid or execute bits | — |
| A gzip member header | Parsed a byte at a time with no length cap, as zlib accepts it | `zlib.decompressobj(31)` |
| The table format | `pretty_format_table`: rows never shortened; `verify`/`parts` fitted to the terminal; control characters escaped | `_cliEmitRows`, `_cliEmitFields`, `_cliTableCells` |
| Long options | A unique prefix is accepted (`--delim`), as argparse did | `_cliParseArgs` |
| `setdefault` with a marker key | Usage error (2), as §21.7.3 asks of a handler | `_ServeStore` |
| Bulk input | Lines with an empty key are skipped (a reader would ignore them) | `_cliStdinBatch` |
| Loose files | Design section 5.6's simple rule; TSVZ.py uses its 3.39 rules instead (§20.2.4 allows both) | — |
| `-V` | `tsvz-rs 0.1.0` | — |

## Where TSVZ.py departs from the spec

Found by the differential harness; tsvz-rs follows the spec. Each is in the harness's `KNOWN`
list until TSVZ.py is fixed.

| Difference | Spec | tsvz-rs | TSVZ.py 4.5 |
|---|---|---|---|
| Exit status of `len` and `keys` when the read stopped at damaged data | §4.10, §20.6: the read failed, exit 1 | 1 | 0 |
