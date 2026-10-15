# tsvz-rs: scrubbing a multi-part store (sub-project 3)

> First drafted 2026-10-13 with a new snapshot part between the prefix and the active part
> (§19.2.3). Revised 2026-10-14, twice, on the user's decisions: the prefix is written as a **new
> part when an ordinal is free** between it and the active part, and compacted **in place into its
> last part** (§19.8) only when none is. "History" at the end says why.

## Intent

`tsvz scrub STORE` compacts a one-part store in place (§19.8). For a multi-part store it writes
the parts no writer appends to any more (the *prefix*: every part but the active one, §17.7) as
a new snapshot part between them and the active part when an ordinal is free there (§19.2),
else compacts them into the last of them, in place; then it applies the store's `#_rotate_#`
action to the prefix parts the snapshot holds. The active part is never touched, so writers keep appending throughout and nothing they
write can be lost or reordered (§19.2.5). TSVZ.py refuses multi-part scrubs, so the conformance
suite's `scrub-multipart` feature and fidelity checks against TSVZ.py *as a reader* (difftest
`--scrub`) are the oracles.

A later release may read a `#_snapshot_#` marker that lets readers skip the history before a
compacted part; it goes to the TSVZ spec first (a draft on a local TSVZ branch) and is not part
of this design.

In this document `§N` is tsvz-spec-v1; "core section N" the core design; "handler section N"
the handler design.

## Decisions

| Question | Decision | Why | Cost if wrong |
|---|---|---|---|
| Which parts are compacted | Every part but the last (the active part, §17.7) | §19.2.1: the writer is on the active part | a writer appending to an earlier part (against §17.7) would race the rewrite; the same holds for any §19.2 implementation |
| Where the compacted rows go | A new part between the prefix and the active part when an ordinal is free there (§19.2.3); else into the prefix's last part, rewritten in place (§19.8) | readers take no lock (TSVZ.py's neither): a new part appears whole under its name (a link), so they see the parts before or after the snapshot, never a part being rewritten. Consecutive ordinals (`.1`, `.2`) leave no room: in place then, as a one-part store is, rather than refused; tsvz-rs readers read again when an earlier part changes under them (store's `open_limited`), TSVZ.py readers can see a torn part. Writers are never quiesced (§19.3) | a store with consecutive ordinals keeps the in-place exposure |
| The snapshot part's ordinal | The midpoint strictly between the prefix's last ordinal and the active one, spelled with the active ordinal's width and letter case; after the unnumbered part 0 alone, from 1 up, never 0 | the midpoint leaves room on both sides for later snapshots; §17.1 names a rotated part 0 `s.tsvz.0.rotated`, the name a rotated part `.0` would take too | another choice is cosmetic |
| How the snapshot part is written | To `<stem>.snapshot-<time>-<pid>.<ext>[.<codec>]` beside the parts (a store of its own, no part of this one), in the codec of the prefix's last part, owner of that part, mode no more open than any prefix part (its owner reads and writes it), synced; checked (alone, and after the whole prefix, it reads as the prefix); the prefix re-stamped; then hard-linked under its name (a link never replaces a file: a name taken meanwhile means try again), the temporary file removed, the directory synced | a reader never sees a half-written part, and no file is ever replaced | a stray temporary file after a crash |
| What the snapshot part holds | Under `keep`, the §19.2.3 snapshot: every prefix part stays before it. Under `rename`/`delete`, the restating snapshot below: a rotation cut short leaves only the first prefix parts before it | a plain snapshot after the first parts only would leave keys they leave live and later parts deleted | a few extra lines per snapshot under rotation |
| What a part compacted in place holds when other prefix parts precede it | A *restating snapshot*: the header comment; `#_version_#` 1; stripping off; a deletion of every key live after any first ones of the earlier parts (the states a rotation cut short leaves, since it removes the newest first); every stateful marker stated explicitly (`#_defaults_#` even when empty, which resets them); the rows, fully resolved; the final stripping and fill-empty settings | under `keep`, or until a `rename`/`delete` completes, readers still read the earlier parts first: their deletions, order and marker state must not leak through. Read after them, after any first ones of them, or alone, the part reads as the whole prefix | a few extra lines per compacted part |
| A one-part prefix (two-part store) | The plain snapshot of core section 4.4 | nothing precedes it | none |
| The in-place rewrite | First the new content goes to a recovery copy beside the part, `<part>.scrub-<time>-<pid>.<ext>`, mode 0600, synced; then the part is rewritten in place (§19.8, core section 4.4); the copy is removed, unless the rewrite left the part damaged, when it is all there is | §19.8's rewrite is not crash-atomic; a crash leaves a complete plain store beside the part. A one-part scrub with no room for the copy (ENOSPC, EDQUOT: scrubbing is how a full disk gets room back) rewrites without one, with a warning; a multi-part scrub, whose guard reads the copy, refuses | a stray recovery file after a crash; no crash safety for a one-part scrub on a full disk |
| Fidelity guard | Before a new part is linked, or a part rewritten, the new content is read alone and after the prefix parts that stay before it; both reads must give the prefix's rows and final markers, or the scrub is refused, nothing written | the rewrite and `rename`/`delete` cannot be undone | two extra reads |
| A prefix read that is incomplete (a part skipped, or a stop at damaged data) | Refused, nothing written | a snapshot of a partial read would hide what follows the damage from every later reader | a damaged prefix must be repaired by hand first |
| Checksum segments in the prefix | Refused, nothing written | a segment open across the prefix/active boundary would no longer verify (§19.4) | such a store cannot be compacted until its segments are gone |
| Parts that declare a spec version newer than 1 (§6.3), in a one-part or a multi-part scrub | Refused, nothing written | this tsvz reads those lines as version 1 only; a rewrite would make that degraded reading permanent for every reader. TSVZ.py writes them back as version 1: a TSVZ.py hole to patch | such a store waits for a tsvz that implements its version |
| A prefix that changes while it is read or written | The rewrite checks the part's stamp (core section 4.4); the other prefix parts are re-stamped before it; a change → try again (3 attempts), then refuse | another process could still rewrite a part in place | none |
| The codec | A new part takes the codec of the prefix's last part; a part compacted in place keeps its own; members of `#_member_size_#` | the store keeps its footprint and convention | none |
| Owner and mode (in place) | The part keeps its owner, and its owner's read and write bits (to rewrite it, and scrub again); its other bits become the intersection of those of all prefix parts (it now holds all their rows), without group access unless they all share its group; set just before the rewrite, and put back only if the part surely holds its old bytes (never after a rewrite that left it damaged) | a private part's rows do not become more readable by mode (a privileged scrub of parts with different owners can still let the target's owner read them: review finding scrub6, deferred) | none |
| `#_rotate_#` | The prefix's final value, applied newest first to the prefix parts the snapshot holds (all of them for a new part, all but the last in place), once the snapshot is in the store: `keep` leaves them; `rename` inserts `.rotated` before the codec suffix (`s.tsvz.1.gz` → `s.tsvz.1.rotated.gz`; part 0 `s.tsvz` → `s.tsvz.0.rotated`, §17.1; a rename cut short between the link and the unlink is finished by the next scrub); `delete` removes them, downgraded to `rename` when the read recovered damage | §19.5; a part whose damage was recovered is never deleted | a mistaken `delete` marker deletes old parts, as the spec says it should |
| Part 0 after `rename`/`delete` | The store path then names no file; reads and appends use the numbered parts (§19.9) | §19.9 | none |
| `clear` of a multi-part store | Unchanged: tombstones appended (§18.8, core section 4.4) | out of scope | none |
| Handler | `scrub` through a handler compacts the same way (the engine calls `maint::scrub`, then reloads) | handler section 4.4 | none |
| Feature claim | `scrub-multipart` in the conformance command, CI and the README | — | none |

## Code

- `parts`: `snapshot_ordinal(prefix_last, active)`, `numbered_path(store, ordinal, suffix)`,
  `rotated_path(path, part0)`.
- `store`: `Store::open_prefix(path, options, count)` reads the first `count` parts with the same
  replay; `Store::read_after(path)` continues a read with one more file (for the guard);
  `Store::newest_version()` is the highest version a `#_version_#` line declared.
- `maint::scrub`: list the parts; stamp the prefix; read it whole; refuse when it is incomplete,
  holds checksums or declares a newer version. Then:
  - an ordinal free → `snapshot_part`: build `snapshot(&s)` under `keep`, else
    `restating_snapshot(&s, &live_after_any_prefix(..))`; encode; `write_snapshot` to
    `snapshot_temp`; re-read the prefix and re-stamp it (changed → try again); `reads_as` and
    `reads_as_after` on the temporary file; link it; sync the directory; `rotate` the prefix;
  - none free → in place: back up a damaged target (core section 4.4); build the content
    (`snapshot` for a one-part prefix, else the restating one); write the recovery copy; check
    it; re-stamp `others`; narrow the target's mode; `rewrite`; remove the recovery copy unless
    the part is damaged; `rotate` `others`.
- `docs/silent-choices.md`: the decisions above that the spec leaves open.

## Testing

- `maint` tests: sparse ordinals (`.3.gz` before the active `.8`) → the new part `.5.gz`, in the
  codec before it, `keep`/`rename`/`delete`; the prefix parts' bytes and inodes unchanged; every
  state a cut-short rotation leaves after a snapshot part reads as the store; after part 0 alone,
  `.1` before `.2`, never `.0`; consecutive ordinals (unnumbered + `.1` + `.2`) → compacted into
  `.1`, rows equal; a deletion, a delete-then-re-add, a defaults reset and a key under a stripping reset
  in the part compacted into keep their meaning after the parts before it and alone; a second
  scrub is `Unchanged`; the guard rejects a plain snapshot that would resurrect a deleted key;
  damaged, checksummed and newer-version prefixes → refused, bytes unchanged; a compressed
  prefix stays compressed; owner and mode (root).
- CLI and handler: `scrub` of a multi-part store through both; appends after the scrub read
  correctly.
- Fidelity against TSVZ.py as the reader: difftest `--scrub` scrubs each store with tsvz-rs and
  checks that TSVZ.py and tsvz-rs read the scrubbed store exactly as they read the original
  (rows, `len`, `keys`, `get`); refusals are tallied by reason, and a refusal by the fidelity
  guard counts as a disagreement.
- `parts` tests: `snapshot_ordinal` (midpoints, width and letter case, no room, UUIDv7 widths,
  never 0 after part 0) and `numbered_path`.
- Conformance: claim `scrub-multipart`; `20.2-scrub-of-a-multipart-store` (part 0 and `.1`: no
  ordinal free, compacted in place) passes, and the `!scrub-multipart` cases are skipped.

## History

The first design wrote the prefix as a new snapshot part with an ordinal strictly between the
prefix and the active part (§19.2.3), and refused a store with consecutive ordinals. The user
rejected refusing such stores, and asked for something smarter: compacting in place needs no
free ordinal, and the recovery copy covers §19.8's lack of crash-atomicity. difftest `--scrub`
then found that compacting into the last part with a plain snapshot lets the earlier parts'
state leak through under `keep` (seed 107: a deleted key came back); the restating snapshot and
the guard's read after the earlier parts are the fix.

The pre-release review then showed that an in-place rewrite of a prefix part tears concurrent
readers, which take no lock: wrong `len` answers, a `clear` that left rows live, a `setdefault`
that overwrote a row. tsvz-rs readers now read again when an earlier part changes under them,
but TSVZ.py readers cannot. The user chose the hybrid: an atomic new part whenever an ordinal is
free, in place only when none is.
