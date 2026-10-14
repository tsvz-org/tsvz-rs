# tsvz-rs: scrubbing a multi-part store (sub-project 3)

> First drafted 2026-10-13 with a new snapshot part between the prefix and the active part
> (§19.2.3). Revised 2026-10-14 on the user's decision: the prefix is compacted **in place into
> its last part** (§19.8), so no new ordinal is needed. "History" at the end says why.

## Intent

`tsvz scrub STORE` compacts a one-part store in place (§19.8). For a multi-part store it compacts
the parts no writer appends to any more (the *prefix*: every part but the active one, §17.7) into
the last of them, in place, then applies the store's `#_rotate_#` action to the other prefix
parts. The active part is never touched, so writers keep appending throughout and nothing they
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
| Where the compacted rows go | Into the prefix's last part, rewritten in place (§19.8): no new ordinal | any ordinals work, consecutive ones included (`.1`, `.2`); no quiescing of writers (§19.3) | none |
| What the compacted part holds when other prefix parts precede it | A *restating snapshot*: the header comment; `#_version_#` 1; stripping off; a deletion of every key the earlier parts leave live; every stateful marker stated explicitly (`#_defaults_#` even when empty, which resets them); the rows, fully resolved; the final stripping and fill-empty settings | under `keep`, or until a `rename`/`delete` completes, readers still read the earlier parts first: their deletions, order and marker state must not leak through. Read after them or alone, the part reads as the whole prefix | a few extra lines per compacted part |
| A one-part prefix (two-part store) | The plain snapshot of core section 4.4 | nothing precedes it | none |
| The rewrite | First the new content goes to a recovery copy beside the part, `<part>.scrub-<time>-<pid>.<ext>`, mode 0600, synced; then the part is rewritten in place (§19.8, core section 4.4); the copy is removed, unless the rewrite left the part damaged, when it is all there is | §19.8's rewrite is not crash-atomic; a crash leaves a complete plain store beside the part | a stray recovery file after a crash |
| Fidelity guard | Before the rewrite, the recovery copy is read alone and after the earlier prefix parts; both reads must give the prefix's rows and final markers, or the scrub is refused, nothing rewritten | the rewrite and `rename`/`delete` cannot be undone | two extra reads |
| A prefix read that is incomplete (a part skipped, or a stop at damaged data) | Refused, nothing written | a snapshot of a partial read would hide what follows the damage from every later reader | a damaged prefix must be repaired by hand first |
| Checksum segments in the prefix | Refused, nothing written | a segment open across the prefix/active boundary would no longer verify (§19.4) | such a store cannot be compacted until its segments are gone |
| Parts that declare a spec version newer than 1 (§6.3), in a one-part or a multi-part scrub | Refused, nothing written | this tsvz reads those lines as version 1 only; a rewrite would make that degraded reading permanent for every reader. TSVZ.py writes them back as version 1: a TSVZ.py hole to patch | such a store waits for a tsvz that implements its version |
| A prefix that changes while it is read or written | The rewrite checks the part's stamp (core section 4.4); the other prefix parts are re-stamped before it; a change → try again (3 attempts), then refuse | another process could still rewrite a part in place | none |
| The compacted part's codec | Its own (plain stays plain), members of `#_member_size_#` | the part keeps its name | none |
| Owner and mode | The part keeps its owner; its mode becomes the intersection of the modes of all prefix parts (it now holds all their rows), without group bits unless they all share its group | a private part's rows never become more readable | none |
| `#_rotate_#` | The prefix's final value, applied to the other prefix parts after the rewrite: `keep` leaves them; `rename` inserts `.rotated` before the codec suffix (`s.tsvz.1.gz` → `s.tsvz.1.rotated.gz`; part 0 `s.tsvz` → `s.tsvz.rotated`); `delete` removes them, downgraded to `rename` when the read recovered damage | §19.5; a part whose damage was recovered is never deleted | a mistaken `delete` marker deletes old parts, as the spec says it should |
| Part 0 after `rename`/`delete` | The store path then names no file; reads and appends use the numbered parts (§19.9) | §19.9 | none |
| `clear` of a multi-part store | Unchanged: tombstones appended (§18.8, core section 4.4) | out of scope | none |
| Handler | `scrub` through a handler compacts the same way (the engine calls `maint::scrub`, then reloads) | handler section 4.4 | none |
| Feature claim | `scrub-multipart` in the conformance command, CI and the README | — | none |

## Code

- `store`: `Store::open_prefix(path, options, count)` reads the first `count` parts with the same
  replay; `Store::read_after(path)` continues a read with one more plain file (for the guard);
  `Store::newest_version()` is the highest version a `#_version_#` line declared.
- `maint::scrub`: one path for both shapes: `prefix` (every part, or every part but the active
  one), `others` (all of the prefix but its last part) and `target` (its last part):
  1. list the parts; stamp the prefix; read it whole; refuse when it is incomplete, holds
     checksums or declares a newer version;
  2. back up a damaged target (core section 4.4);
  3. build `snapshot(&s)` when `others` is empty, else read `others` whole and build
     `restating_snapshot(&s, &earlier)`;
  4. narrow the target's mode; write the recovery copy; check it with `reads_as` and
     `reads_as_after`; re-stamp `others` (changed → try again);
  5. `rewrite(target, content, stamp, codec, member_size, Some(&recovery))`; remove the recovery
     copy unless the part is damaged;
  6. apply `#_rotate_#` to `others`; `Outcome::Written` (or `Partial`, `Unchanged`).
- `docs/silent-choices.md`: the decisions above that the spec leaves open.

## Testing

- `maint` tests: unnumbered + `.1` + `.2` → compacted into `.1`, rows equal, `keep`/`rename`/
  `delete`; a deletion, a delete-then-re-add, a defaults reset and a key under a stripping reset
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
- Conformance: claim `scrub-multipart`; `20.2-scrub-of-a-multipart-store` passes, and the
  `!scrub-multipart` cases are skipped.

## History

The first design wrote the prefix as a new snapshot part with an ordinal strictly between the
prefix and the active part (§19.2.3), and refused a store with consecutive ordinals. The user
rejected refusing such stores, and asked for something smarter: compacting in place needs no
free ordinal, and the recovery copy covers §19.8's lack of crash-atomicity. difftest `--scrub`
then found that compacting into the last part with a plain snapshot lets the earlier parts'
state leak through under `keep` (seed 107: a deleted key came back); the restating snapshot and
the guard's read after the earlier parts are the fix.
