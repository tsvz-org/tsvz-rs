# Where tsvz-spec-v1 is silent

tsvz-rs follows `tsvz-spec-v1.md` wherever it decides. Where it leaves a choice open, tsvz-rs
does what TSVZ.py does, so the two tools agree in practice. Each choice is listed here with the
TSVZ.py code it copies. The differential harness (`difftest/`) finds new ones.

| Choice | tsvz-rs (as TSVZ.py) | TSVZ.py reference |
|---|---|---|
| `parts` ordinal | Printed as written in the file name (`002`, `B`), not normalized | `_partRows` |
| xz parts | A member may be an `.xz` stream or a legacy `.lzma` stream (auto-detected) | `lzma.LZMADecompressor()` |
| xz stream padding | Zero bytes after an xz stream are judged a member cut short (a warning; every line is read, under any policy) | `decompress_items` |
| zstd skippable frames | Decoded as frames with no output | `ZstdDecompressor` |
| A corrupt member's output (`recover`) | What its bytes decode, fed one at a time, before the byte that fails; what that byte decodes is dropped with it | `_decodeAlone` |
| Row width | Rows, and a missing key's defaults, padded to the store's width (§3.6 lets a reader pad): the count of the defaults active at the first data row when there are any, else that row's written cells. A longer row keeps every cell | `_specPadRow` |
| Appending after a torn tail | The uncommitted bytes after the last newline are cut first, with a warning, so the new record cannot join them | `_lockedAppend(…, 'truncate')` |
| `scrub` markers | `#_version_#` written as `1`; then strip, fill-empty, return-defaults, rotate, write-ack, member-size and on-corruption when not default; `#_defaults_#` last | `_specPreamble` |
| `scrub` rows | Fully resolved, padded with empty cells to the final defaults' width; the §19.4 compensating layout only when a row would not read back the same | `_specScrubCells`, `_specRoundTrips` |
| `scrub` and checksum markers | Not kept: a snapshot has no segments to verify | `_specScrub` |
| `scrub` of a multi-part store (§19.2, §19.8; TSVZ.py refuses it) | The parts before the active one are compacted in place into the last of them (§19.8), which no writer appends to (§17.7): no new part, so any ordinals work. Then `#_rotate_#` applies to the others (`rename`: `.rotated` before the codec suffix, part 0 → `s.tsvz.rotated`; `delete` is downgraded to `rename` when the read recovered damage) | — |
| What the compacted part holds when other parts precede it | The header comment, `#_version_#` 1, stripping off, a deletion of every key those parts leave live, every stateful marker stated explicitly (`#_defaults_#` even when empty), the rows fully resolved, then the final stripping and fill-empty settings: it reads as the whole prefix after those parts (`keep`, or before a rotation completes) and alone | — |
| A multi-part scrub of parts that cannot be read whole, or that hold checksum segments | Refused, nothing written: a snapshot of a partial read would hide the rest from later readers, and a checksum segment open into the active part would no longer verify | — |
| `scrub` of parts that declare a spec version newer than 1 (§6.3) | Refused, exit 1, nothing written: tsvz-rs reads those lines as version 1 only, and a rewrite would make that reading permanent for every reader (TSVZ.py writes them back as version 1, a hole to patch there) | `_specPreamble` |
| How the compacted part is written | The new content first goes to a private (0600) recovery copy beside the part, `<part>.scrub-<time>-<pid>.<ext>`, synced, and must read as the parts it replaces both alone and after the parts before it, or the scrub is refused; then the part is rewritten in place, and the copy removed unless the rewrite left the part damaged. The part keeps its owner; its mode becomes the intersection of the modes of all the parts it holds rows of, without group bits unless they all share its group | — |
| `clear` of one part | Header comment and markers, without `#_version_#` | `_specClearTabularFile` |
| A path that names one part (`s.tsvz.1`, §20.1.3) | `clear` and `scrub` refuse it, exit 1, nothing written; other operations treat it as a store of its own | `_specClearTabularFile`, `_specScrub` |
| Space for an in-place rewrite | Reserved first with `posix_fallocate` on Linux (§19.8's SHOULD); if a write still fails, the whole old part is put back, and if that fails too the error says the part is damaged. macOS has the restore only | `_specRewriteInPlace` |
| A damaged part before a rewrite | Copied to `<part>.damaged-<YYYYmmddTHHMMSS>` (`.1`, `.2`, … when taken) and synced | `_specBackupPart` |
| The backup copy's permissions | The part's read bits plus owner write (`mode & 0o644`, less the umask), so a private store's copy stays private and never carries setuid, setgid or execute bits; the part's owner and group where allowed, else, left in another group, no group bits | `_specBackupPart`, `_matchOwner` |
| A gzip member header | Parsed a byte at a time with no length cap, as zlib accepts it | `zlib.decompressobj(31)` |
| xz and lzma decoder memory | At most 1 GiB; a member whose header demands more (a legacy `.lzma` header can declare a 4 GiB dictionary) is judged corrupt. TSVZ.py's `lzma` has no limit | `lzma.LZMADecompressor()` |
| The table format | `pretty_format_table`: rows never shortened; `verify`/`parts` fitted to the terminal; control characters escaped | `_cliEmitRows`, `_cliEmitFields`, `_cliTableCells` |
| Long options | A unique prefix is accepted (`--delim`), as argparse did | `_cliParseArgs` |
| `setdefault` with a marker key | Usage error (2), as §21.7.3 asks of a handler | `_ServeStore` |
| Bulk input | Lines with an empty key are skipped (a reader would ignore them) | `_cliStdinBatch` |
| A part that cannot be read | Skipped with a warning: reads print what was read and exit 1 (§21.8.5 for a handler, and the same on the files so that routing changes nothing); `pop`, `popitem`, `setdefault`, `clear`, `scrub` and a `delete` that must name a key (§20.2.7) change nothing | `_specLoad` (`'unreadable'`) |
| `--x-written` | A handler accepts it: the write is answered once written, and a failed write answers status 1. A routed `set`, `append` or `delete` sends it, so that a failed write exits 1 as on the files; so does every write of `handler::Client` | `_serveDispatch`, `_cliRouted`, `TSVZClient` |
| Routed operations | Every operation but `serve`, `stop`, `verify` and `parts` (which read the files themselves) | `_cliHandler` |
| A routed operation whose handler goes away | `read`, `get`, `has`, `len`, `keys` warn and run on the files; any other exits 1, saying it may or may not have been applied | `_cliRouted` |
| Handler statuses | A reading operation on a missing store: 1, "no such store"; a read that skipped a part or stopped: 1, "could not read every part of the store", with what was read | `_serveDispatch` |
| `pop`, `popitem`, `setdefault` through a handler | Answered once their write is made; a failed write answers 1 and no row | `_ServeStore.pop` |
| Writes that fail after they were acknowledged | Reported on `serve`'s standard error; every response carries a `#!` warning until a batch is written again | `_ServeWriter`, `_serveRespond` |
| The handler's socket | A new `tsvz-XXXXXX` directory under `$XDG_RUNTIME_DIR` when it is absolute and writable and access is private, else the temporary directory, made absolute; socket 0600, 0660 with `--x-group`, or `--x-mode`; the directory 0700 plus search bits for whom the socket admits | `_ServeHandle.start` |
| The pointer file | Created 0600, published 0644; one that is incomplete is read again after 50 ms; one with another first line, or no `address` or `host`, means no handler, without a warning | `_ServeHandle.lock`, `_serveLookup` |
| Request line limit | 64 MiB; a longer line closes the connection | `_SERVE_LINE_LIMIT` |
| `stop STORE` | After `#0`, waits up to 10 s until the pointer file no longer names that handler | `_cliStop` |
| `serve` options | `--x-idle-timeout S`, `--x-group G`, `--x-mode M`, and `--x-direct` for any command, spelled as TSVZ.py spells them | `_CLI_OPTIONS` |
| `-v` with a handler | A routed command names the handler it used; `serve -v` reports each request's status | — |
| Strict stores only (departs from TSVZ.py) | `serve` refuses a loose file and the CLI never forwards one: tsvz-rs and TSVZ.py read loose files by different rules, so a forwarded command could print what the other tool's files would not | — |
| Loose files | Design section 5.6's simple rule; TSVZ.py uses its 3.39 rules instead (§20.2.4 allows both), which also cut a row longer than the first to its width | — |
| `-V` | `tsvz-rs 0.1.0` | — |

## Where TSVZ.py departs from the spec

Found by the differential harness; tsvz-rs follows the spec. Each is in the harness's `KNOWN`
list until TSVZ.py is fixed. None is known as of TSVZ.py 4.6, which fixed the exit status of
`len` and `keys` after a read stopped at damaged data (§4.10, §20.6).
