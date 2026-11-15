# Lessons: full entries

Why this project's APIs, formats and agreed behaviours had to change. `lessons.md` has one line per
entry: read that first, and look entries up here by date and name rather than reading this whole. New
entries go last, in the same commit as the change:

```markdown
## 2026-11-15 Loose files are read and rewritten by TSVZ.py's rules where the two can agree
- Changed: tsvz-rs takes a loose file's column count from its first keyed line, and reads a key
  alone, or with only blank cells, as a deletion but a row in a one-column file, as TSVZ.py does.
  Its `scrub` of a loose file keeps the lines in their places (`#_defaults_#` lines included)
  instead of writing its rows anew; `clear` keeps the `#_defaults_#` lines in force; a delete in a
  one-column file rewrites the file without the key's lines.
- Why we had to change: tsvz-rs's `scrub` and `clear` of a TSVZ.py-managed `.tsv` dropped its
  `#_defaults_#` lines and a one-column file's keys, and tsvz-rs listed keys TSVZed had deleted
  (TSVZed writes a deletion as `key<TAB><TAB>…`). The user picked "Fix both in 0.1.4 (Recommended)"
  (2026-11-14); reading blank cells as a deletion was chief's decision (2026-11-15).
- Original design was wrong because: it read "the data semantics of loose files are
  implementation-defined" (§20.2.4) as room for a simpler rule of its own, on files that another
  program writes and reads by its rules. Each tool's rewrite then changed what the other read:
  tsvz-rs's scrub lost TSVZ.py's defaults and one-column keys, and TSVZ.py's scrub reordered or
  revived keys for tsvz-rs.
- Lesson: when you read and rewrite files another program owns, read them by its rules wherever
  you can, and let a rewrite keep lines in their places, changing one only where every reader reads
  the result alike; test that with the other program as the oracle.
