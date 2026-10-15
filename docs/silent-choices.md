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
| `scrub` of a multi-part store (§19.2, §19.8; TSVZ.py refuses it) | The parts before the active one (no writer appends to them, §17.7) become a new part, its ordinal the midpoint strictly between the last of them and the active part, spelled with the active ordinal's width and letter case (after part 0 alone, never 0: §17.1 names a rotated part 0 `s.tsvz.0.rotated`, as a rotated part `.0` would be); when no ordinal is free, they are compacted in place into the last of them (§19.8), with a warning that readers taking no lock may see it rewritten and that sparse ordinals avoid it, and writers are never quiesced. Then `#_rotate_#` applies to the parts the snapshot holds, newest first (`rename`: `.rotated` before the codec suffix, part 0 → `s.tsvz.0.rotated` as §17.1 names it; a rename cut short by a crash, both names left, is finished by the next scrub; `delete` is downgraded to `rename` when the read recovered damage) | — |
| When a multi-part scrub writes nothing | When the last part before the active one already reads alone as all of them and is no larger than a new snapshot would be (a scrub run again, by cron say, would add a full copy each time); it then only finishes a rotation cut short. A file system without hard links (FAT, some network and FUSE mounts) gets the in-place compaction, with a warning saying why | — |
| What a snapshot part holds | Under `#_rotate_# keep`, the §19.2.3 snapshot (every part before it stays before it). Under `rename` or `delete`, and when compacting in place into a part that other parts precede: the header comment, `#_version_#` 1, stripping off, a deletion of every key live after any first ones of those parts (what a rotation cut short leaves, since it removes the newest first), every stateful marker stated explicitly (`#_defaults_#` even when empty), the rows fully resolved, then the final stripping and fill-empty settings: it reads as the whole prefix after those parts, after any first ones of them, and alone | — |
| A multi-part scrub of parts that cannot be read whole, or that hold checksum segments | Refused, nothing written: a snapshot of a partial read would hide the rest from later readers, and a checksum segment open into the active part would no longer verify | — |
| An integer marker value too large for 64 bits (`#_version_#`, `#_member_size_#`) | Still an integer (§12.4): the largest 64-bit value, so a version above every one implemented, and a member size beyond any part (TSVZ.py 4.8 does so for `#_version_#`; a member size it cannot convert is ignored) | `_specApplyMarker` |
| How a snapshot is written | A new part: written to `<stem>.snapshot-<time>-<pid>-<n>.<ext>[.<codec>]` beside the parts (a store of its own, no part of this one), with the owner of the last part it holds and a mode no more open than any of them (its owner reads and writes it), synced, checked to read as the parts it holds alone and after all of them, then hard-linked under its name (a link never replaces a file; a name taken meanwhile means try again) and the directory synced. In place: the new content first goes to a private (0600) recovery copy beside the part, `<part>.scrub-<time>-<pid>.<ext>` (a one-part scrub with no room for it rewrites without one, with a warning, as TSVZ.py does, on a file system that overwrites in place — ext2/3/4, XFS, f2fs, tmpfs on Linux; elsewhere, copy-on-write ones included, and for a multi-part scrub, it refuses), synced, and must read as the parts it replaces both alone and after the parts before it, or the scrub is refused; then the part is rewritten in place, and the copy removed unless the rewrite left the part damaged. The part keeps its owner, and its owner's read and write bits; its other bits become the intersection of those of all the parts it holds rows of, without group access unless they all share its group; set just before the rewrite, and put back only if the part surely holds its old bytes. Either way, when the part the rows go into would belong to a user (not the one scrubbing) who could not read every part they come from, by its mode, the scrub is refused and nothing is written |
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
| `-V` | `tsvz-rs` and the crate version (`tsvz-rs 0.1.1`) | — |

## Where TSVZ.py departs from the spec

Found by the differential harness; tsvz-rs follows the spec. Each is in the harness's `KNOWN`
list until TSVZ.py is fixed. None is known as of TSVZ.py 4.6, which fixed the exit status of
`len` and `keys` after a read stopped at damaged data (§4.10, §20.6).
