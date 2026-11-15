# tsvz-rs: progress

Open work lives here and nowhere else (RULES, "Long work: the progress file"). Edit rows in place, in the commit that changes the work.

## Goal
- 2026-11-05, for every cas tool: "we improve the cas scripts iteratively. feel free to add scope / tasks if when doing some work, you noticed some other thing that can be improved with cas scripts." (the user; no goal for this repo alone is written in the repo)
- The specs state the purpose in the design's own words, not the user's: "A fast, standalone implementation of tsvz-spec-v1 in Rust: a `tsvz` binary that does what TSVZ.py's `tsvz` does for spec stores, much faster and without Python, built on a library crate that other Rust programs can embed" (docs/superpowers/specs/2026-10-12-tsvz-rs-core-design.md, Intent, 2026-10-12).

## Open work

### tsvz-rs 0.1.4, the next release (no written plan; commit messages of 4afd4ab and e76cdcf; docs/limitations.md, entry "A lone key is a deletion to tsvz-rs at one column")
- **Building (started 2026-11-14):** branch feat/014 (worktree tsvz-rs-014), with the simplify pass below; the rows of this section are in the build.
- **Release what main holds since v0.1.3: snapshot parts (§19.10) and the `#_snapshot_#` refusal.** A read of a strict store begins at the last snapshot part, a multi-part scrub marks the parts it writes, and a writer refuses `#_snapshot_#` in a strict store, as TSVZ.py 4.9 does (commits 48ab4ee, 4afd4ab, e76cdcf, 2026-10-18; README and docs/silent-choices.md already describe them). State: merged, not released. Where: main (c46c46e) has these and cd406ff (the tsvz-org repository URL, which crates.io takes only from the next publish); Cargo.toml still says 0.1.3, no tag after v0.1.3, crates.io's newest is 0.1.3. CI on main was green (2026-10-18 and 2026-11-09). Next: bump the version, review the release (the review of 4afd4ab ended in the fixes of e76cdcf; no re-review of those fixes is recorded in the repo), `cargo publish`, tag, GitHub release with notes. From: the main session's plan (no pick of the user's on file); e76cdcf already calls a scrub of 0.1.3 the old one.
- **Read a lone key (a key with no value) as a row at one column, as TSVZ.py and TSVZ 3.39 do.** Today tsvz-rs reads it as a deletion, so after a TSVZ.py rewrite of a loose file it can list the same keys in another order (the entry's examples: a `TSVZed` commit under header `q`, and a scrub with defaults). State: not started; in 0.1.4 (the user's pick 2026-11-14, "Fix both in 0.1.4 (Recommended)", which reopens their 2026-11-04 "Leave it, document (Recommended)"). Where: no code for it yet. Next: turn the entry's two examples into tests against TSVZ.py as the oracle, then change `loose_line` (src/replay.rs) and the loose scrub. The main session's notes add that tsvz-rs's scrub and tombstone deletes do not match TSVZ.py's compaction (this predates TSVZ 5.10); nothing else for that is written down. From: the 2026-11-09 deferral and the user's pick of 2026-11-14.
- **Write down in docs/silent-choices.md ("Loose files" row) that a loose row whose cells after the key are all empty (`b\t`, `b\t\t`) is a present row in tsvz-rs and a deletion to TSVZ.py's reader (TSVZ.py `_processLine`), unless the file has one column.** State: not started (docs only). Where: README or silent-choices change with the release above. From: the main session's note of 2026-11-09 ("tsvz-rs reads `key\t` in loose files as present-empty where TSVZ legacy readers read a deletion; note for tsvz-rs").

- **Loose-file `scrub` and `clear` of a `.tsv` that TSVZ.py manages drop its `#_defaults_#` lines** (tsvz-rs reads every `#` line as a comment), so the rewritten file reads differently to TSVZ.py. State: not started; in 0.1.4. Where: keep `#_defaults_#` lines through a loose rewrite, with a TSVZ.py-oracle test, and update the docs/silent-choices.md row ("Loose files rewritten") when it lands. The keys a one-column file loses are the lone-key row above. From: the user's pick 2026-11-14, "Fix both in 0.1.4 (Recommended)", on their 2026-11-04 pick "Leave it, document (Recommended)" for: "tsvz-rs (the Rust TSVZ) doesn't follow TSVZ.py's legacy .tsv rules: its scrub and clear of a TSVZ.py-managed .tsv drop #_defaults_# lines and a 1-column file's keys."

### The simplify pass (no written plan)
- **Simplify and drop dead weight, as the other cas tools had.** State: not started; with or before 0.1.4. Where: remove what no operator needs (dead code, guards for states that cannot happen, extra layers), behaviour unchanged, reviewed like a release. From: the user, 2026-10-23: "maybe we should go through the repos after this round, then simplify and drop any stuff that are dead weight?"; for firewatcher, simple-iotest and tsvz-rs, "Run it on all three (Recommended)", 2026-11-14.

### Standing rows
- **Deferred work** is in `docs/limitations.md` (entries of kind "deferred"); each names when it is revisited. Two entries today: readers take no lock (TSVZ 5.3's read lock and gate), revisited when someone shares a file between tsvz-rs and another writer that rewrites it; and the lone key above.

## Standing decisions
The user's rulings on how the cas repos are worked, one line each; a line changes only on the user's word. The cas monorepo's CLAUDE.md holds the same list, with how to reach the test machines.
- Deferrals (2026-11-05): "please do not ask me about features you think should defer to next version anymore. instead, note them down and after getting the current version done, start the next version right away. only ask me to approve things that are going into limitations that we do not have plans to fix it any time soon."
- The release gate (2026-11-09): "releasing a new version gate is only if it does not make an existing issue worse. even if the fix does not completely resolve the problem, if it makes the result more tolerable, we shall not gate it for release because there are still issues."
- Review before upload (2026-10-18): "commit and push is fine but when packaging, please follow the rule that need a code review before uvpack / upload to pypi".
- A test that passed only on a rerun (2026-11-06, "Green with a warning (Recommended)"): CI stays green with a warning, and "Before any upload, every such test still needs a written cause either way."
- Release pushes (2026-11-10, "Release OK covers it (Recommended)"): an OK for a release covers its push of main, the version tag and the GitHub release.
- CentOS 7 (2026-11-04): "for centos 7, which is already EOL, we only target all the last packages released for it. this also includes EPEL which is also in archive right now."
- EL8 (2026-11-05): "yes rocky 8 it is. let's also only test on them when you think test on real kernel is needed."
- macOS (2026-11-11): tests run on the user's own always-on Mac, as a standard user made for them, never with sudo.

## Questions waiting for the user
- None.

## Closed
- Core crate and CLI, write handler (§21), multi-part scrub (§19.2, §19.8): every part shipped in v0.1.0 to v0.1.3 (crates.io and GitHub releases); the designs and plans are omitted here.
- Docs branches docs/tsvz-55-loose, docs/tsvz-53-read-locks-deferred and docs/lone-key-order: merged into origin/main by PR #1 (47cca88) and PR #2 (c46c46e), 2026-11-06 and 2026-11-09.
