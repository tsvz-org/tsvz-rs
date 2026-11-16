# Lessons: full entries

Why this project's APIs, formats and agreed behaviours had to change. `lessons.md` has one line per
entry: read that first, and look entries up here by date and name rather than reading this whole. New
entries go last, in the same commit as the change:

```markdown
## YYYY-MM-DD <short name>
- Changed: ...
- Why we had to change: ...
- Original design was wrong because: ...
- Lesson: ...
```

## 2026-11-15 Loose files are read and rewritten by TSVZ.py's rules where the two can agree
- Changed: tsvz-rs takes a loose file's column count from its first keyed line, and reads a key
  alone, or with only blank cells, as a deletion but a row in a one-column file, as TSVZ.py does.
  Its `scrub` of a loose file keeps each line it keeps as the file has it (`#_defaults_#` lines
  included) instead of writing its rows anew; `clear` keeps the `#_defaults_#` lines in force
  (none in a file of one column); a delete decides from the file's column count alone, as TSVZ.py
  does, and in a one-column file rewrites the file without the key's lines. Line 1's byte-order
  mark is part of its key.
- Why we had to change: tsvz-rs's `scrub` and `clear` of a TSVZ.py-managed `.tsv` dropped its
  `#_defaults_#` lines and a one-column file's keys, and tsvz-rs listed keys TSVZed had deleted
  (TSVZed writes a deletion as `key<TAB><TAB>…`). Fixing both was asked for in 0.1.4 (2026-11-14).
- Original design was wrong because: it read "their data semantics are implementation-defined"
  (§20.2.4) as room for a simpler rule of its own, on files that another program writes and reads
  by its rules. Each tool's rewrite then changed what the other read: tsvz-rs's scrub lost TSVZ.py's
  defaults and one-column keys, and TSVZ.py's scrub reordered or revived keys for tsvz-rs. The first
  fix then added a rule TSVZ.py does not have, to write a delete only for a key that is live: it read
  the whole file, and misjudged every key the two readers read differently (a trailing space, a
  byte-order mark, a reset of `#_defaults_#`).
- Lesson: when you read and rewrite files another program owns, read them by its rules wherever
  you can, and let a rewrite keep lines as they are, changing one only where every reader reads the
  result alike; decide a write from the same input its rules decide from, since a state they do not
  have (whether a key is live) is one your reading can get wrong; test that with the other program
  as the oracle.
