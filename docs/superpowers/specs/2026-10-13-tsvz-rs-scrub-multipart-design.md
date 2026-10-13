# tsvz-rs: scrubbing a multi-part store (sub-project 3)

> Draft written overnight 2026-10-13 while the user was away, on their instruction to work
> independently on the remaining features. Every choice below that the spec leaves open is
> marked **Ruling** and is the user's to confirm or change. The implementation lives on the
> local branch `scrub-multipart` (based on `handler`); discard the branch to discard all of it.

## Intent

`tsvz scrub STORE` compacts a one-part store in place (§19.8) and refuses a multi-part store
today ("this tsvz does not compact multi-part stores"). This sub-project compacts a multi-part
store the way §19.2 recommends: the parts no writer appends to any more (the *immutable
prefix*) are replayed and written as one new snapshot part, slotted between them and the active
part; then the store's `#_rotate_#` action is applied to the prefix. The active part is never
touched, so writers keep appending throughout and nothing they write can be lost or reordered
(§19.2.5). TSVZ.py refuses multi-part scrubs, so the conformance suite's `scrub-multipart`
feature and fidelity checks against TSVZ.py *as a reader* are the oracles.

In this document `§N` is tsvz-spec-v1; "core section N" the core design; "handler section N"
the handler design.

## Decisions

| Question | Ruling (default) | Why | Cost if wrong |
|---|---|---|---|
| Which parts are compacted | Every part but the last (the active part, §17.7) | §19.2.1: the prefix is lowest ordinal through N, the writer is on the active part | a store whose writers append to an earlier part (against §17.7) loses those appends to the snapshot's later ordinal; the same holds for any §19.2 implementation |
| Where the snapshot goes | A new numbered part with ordinal S, N < S < A (A = active ordinal; the unnumbered part 0 counts as below every ordinal), S = the midpoint, spelled with A's width and letter case | §19.2.3; the midpoint leaves room on both sides for later snapshots | another choice of S is cosmetic |
| No free ordinal (A = N + 1) | Refuse: exit 1, nothing written, a message naming both parts and suggesting sparse ordinals (§17.6) | §19.3 allows quiescing writers instead, but a writer that resolved the old active part before the snapshot and is waiting on its lock would append behind the snapshot, and its record would be ordered before it: lost under `rename`/`delete`, overridden otherwise | a store with consecutive ordinals cannot be scrubbed until it gets a gap |
| A read of the prefix that is incomplete (a part skipped, or a stop at damaged data) | Refuse, nothing written | a snapshot of a partial read would hide what follows the damage from every later reader | a damaged prefix must be repaired by hand first |
| A checksum segment open across the prefix/active boundary | Refuse, nothing written | the active part's closing marker covers bytes the snapshot moves: `verify` would report a mismatch afterwards (§19.4 fidelity) | rare; such a store waits until its segment closes |
| The snapshot's codec | The codec of the prefix's last part (plain when it is plain), members of `#_member_size_#` | keeps the store's footprint and convention | none |
| How the snapshot is written | Into a temporary file in the store's directory whose name matches no part, fsync'd, then linked to the final name (`link` fails if the name exists; never renamed over a part, §18.7), the temporary unlinked, the directory fsync'd. Owner and group of the prefix's last part (§18.7's SHOULD); mode the intersection of every prefix part's mode (the snapshot holds all their rows), without group bits unless they all share its group; set before the file gets its final name | a reader never sees a half-written part; no existing file is ever replaced; a private part's rows never become more readable | none |
| A prefix that changes while it is read or the snapshot written | Remove the snapshot and try again (3 attempts, as single-part scrub), then refuse | §19.2 assumes immutability; another process could still rewrite a part in place | none |
| Fidelity check | Before rotating, read the snapshot alone and compare its rows and final markers with the prefix read; a difference removes the snapshot and refuses (a bug guard) | `rename`/`delete` cannot be undone | an extra read of the snapshot |
| `#_rotate_#` | The prefix's final marker value: `keep` leaves the parts; `rename` inserts `.rotated` before the codec suffix (`s.tsvz.1.gz` → `s.tsvz.1.rotated.gz`; part 0 `s.tsvz` → `s.tsvz.rotated`); `delete` removes them. Applied only after the snapshot is durable and checked | §19.5; no downgrade, since the store's owner chose the action | a mistaken `delete` marker deletes old parts, as the spec says it should |
| Part 0 after `rename`/`delete` | The store path then names no file; reads and appends use the numbered parts (§19.9) | §19.9: writers append only to numbered parts afterwards | none |
| `clear` of a multi-part store | Unchanged: tombstones appended (§18.8, core section 4.4) | out of scope | none |
| Handler | `scrub` through a handler compacts the same way (the engine calls `maint::scrub`, then reloads) | handler section 4.4 | none |
| Feature claim | `scrub-multipart` in the conformance command, CI and the README | — | none |

## Code

- `parts`: `ordinal_between(prefix_last: Option<&Part>, active: &Part) -> Option<String>`; the
  rotated name of a part.
- `store`: `Store::open_prefix(path, options, count)` (or an internal `Options` limit) reading
  the first `count` parts only, with the same replay; the follow state is not needed.
- `maint::scrub`: one part → today's in-place path; two or more → `snapshot_prefix`:
  1. list the parts; refuse when there is no free ordinal; stamp the prefix;
  2. read the prefix whole (skipping off); refuse when incomplete or a checksum segment is open
     at its end;
  3. build `snapshot(&prefix)` (core: the same builder as in-place scrub), encode it with the
     codec, write the temporary file, fsync, copy owner and mode, link to the final name;
  4. re-stamp the prefix: changed → remove the snapshot, try again;
  5. read the snapshot alone and compare with the prefix read; different → remove it, refuse;
  6. fsync the directory; apply `#_rotate_#`;
  7. `Outcome::Written`.
- `docs/silent-choices.md`: the rulings above that the spec leaves open (ordinal choice, no
  quiescing, refusals, codec, rotate of part 0).

## Testing

- Unit: `ordinal_between` (part 0 + `.1` → `0`; `1`/`5` → `3`; `0f`/`10` → none; widths and
  letter case kept; UUIDv7-width ordinals); the rotated name; `open_prefix`.
- `maint` tests: unnumbered + `.1` → `.0` written, rows equal, `keep`/`rename`/`delete`; no room
  → refused, bytes unchanged; damaged prefix → refused; a checksum segment open across the
  boundary → refused; a compressed prefix → compressed snapshot; owner and mode kept (root);
  the conformance store of `20.2-scrub-of-a-multipart-store`.
- CLI and handler: `scrub` of a multi-part store through both; appends after the scrub read
  correctly.
- Fidelity against TSVZ.py as the reader: difftest `--scrub` scrubs each multi-part seed with
  tsvz-rs and checks that TSVZ.py reads the scrubbed store exactly as it read the original
  (rows, `len`, `keys`, `get`), for `keep`, `rename` and `delete`.
- Conformance: claim `scrub-multipart`; `20.2-scrub-of-a-multipart-store` passes, and the
  `!scrub-multipart` cases are skipped.
