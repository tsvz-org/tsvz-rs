# tsvz-rs: progress

Open work lives here and nowhere else (RULES, "Long work: the progress file"). Edit rows in place, in the commit that changes the work.

## Goal
- 2026-11-05, for every cas tool: "we improve the cas scripts iteratively. feel free to add scope / tasks if when doing some work, you noticed some other thing that can be improved with cas scripts." (the user; no goal for this repo alone is written in the repo)
- The specs state the purpose in the design's own words, not the user's: "A fast, standalone implementation of tsvz-spec-v1 in Rust: a `tsvz` binary that does what TSVZ.py's `tsvz` does for spec stores, much faster and without Python, built on a library crate that other Rust programs can embed" (docs/superpowers/specs/2026-10-12-tsvz-rs-core-design.md, Intent, 2026-10-12).

## Open work

### tsvz-rs 0.1.4, the next release (no written plan; commit messages of 4afd4ab and e76cdcf; docs/limitations.md, entry "A lone key is a deletion to tsvz-rs at one column")
- **Release what main holds since v0.1.3: snapshot parts (§19.10) and the `#_snapshot_#` refusal.** A read of a strict store begins at the last snapshot part, a multi-part scrub marks the parts it writes, and a writer refuses `#_snapshot_#` in a strict store, as TSVZ.py 4.9 does (commits 48ab4ee, 4afd4ab, e76cdcf, 2026-10-18; README and docs/silent-choices.md already describe them). State: merged, not released. Where: main (c46c46e) has these and cd406ff (the tsvz-org repository URL, which crates.io takes only from the next publish); Cargo.toml still says 0.1.3, no tag after v0.1.3, crates.io's newest is 0.1.3. CI on main was green (2026-10-18 and 2026-11-09). Next: bump the version, review the release (the review of 4afd4ab ended in the fixes of e76cdcf; no re-review of those fixes is recorded in the repo), `cargo publish`, tag, GitHub release with notes. From: the main session's plan (no pick of the user's on file); e76cdcf already calls a scrub of 0.1.3 the old one.
- **Read a lone key (a key with no value) as a row at one column, as TSVZ.py and TSVZ 3.39 do.** Today tsvz-rs reads it as a deletion, so after a TSVZ.py rewrite of a loose file it can list the same keys in another order (the entry's examples: a `TSVZed` commit under header `q`, and a scrub with defaults). State: not started. Where: no code for it; the entry says "Revisit when: the next tsvz-rs version", so it rides with the release above or the one after. Next: turn the entry's two examples into tests against TSVZ.py as the oracle, then change `loose_line` (src/replay.rs) and the loose scrub. The main session's notes add that tsvz-rs's scrub and tombstone deletes do not match TSVZ.py's compaction (this predates TSVZ 5.10); nothing else for that is written down. From: the main session's plan of 2026-11-09, recorded as a deferral under the user's standing rule that deferrals are noted, not asked.
- **Write down in docs/silent-choices.md ("Loose files" row) that a loose row whose cells after the key are all empty (`b\t`, `b\t\t`) is a present row in tsvz-rs and a deletion to TSVZ.py's reader (TSVZ.py `_processLine`), unless the file has one column.** State: not started (docs only). Where: README or silent-choices change with the release above. From: the main session's note of 2026-11-09 ("tsvz-rs reads `key\t` in loose files as present-empty where TSVZ legacy readers read a deletion; note for tsvz-rs").

- **Loose-file `scrub` and `clear` of a `.tsv` that TSVZ.py manages drop its `#_defaults_#` lines** (tsvz-rs reads every `#` line as a comment), so the rewritten file reads differently to TSVZ.py. State: not started. Where: keep `#_defaults_#` lines through a loose rewrite, with a TSVZ.py-oracle test; today it is only a row in docs/silent-choices.md ("Loose files rewritten"). The keys a one-column file loses are the lone-key row above. From: the 2026-11-14 progress sweep; a fix for the next release (a deferral, not asked).

### Standing rows
- **Deferred work** is in `docs/limitations.md` (entries of kind "deferred"); each names when it is revisited. Two entries today: readers take no lock (TSVZ 5.3's read lock and gate), revisited when someone shares a file between tsvz-rs and another writer that rewrites it; and the lone key above.

## Questions waiting for the user
- None.

## Closed
- Core crate and CLI, write handler (§21), multi-part scrub (§19.2, §19.8): every part shipped in v0.1.0 to v0.1.3 (crates.io and GitHub releases); the designs and plans are omitted here.
- Docs branches docs/tsvz-55-loose, docs/tsvz-53-read-locks-deferred and docs/lone-key-order: merged into origin/main by PR #1 (47cca88) and PR #2 (c46c46e), 2026-11-06 and 2026-11-09.
