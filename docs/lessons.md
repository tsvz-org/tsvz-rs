# Lessons

What this project's past changes taught us. Read all of this before changing an API, a format or an
agreed behaviour here. For the full story of a line, find the entry with the same date and name in
`lessons-full.md`.

When one has to change, add the full entry to `lessons-full.md` and one line here, newest last, in the
same commit as the change. Never merge lines to save space; before this file passes 80 KB (what one
Claude Code read returns, measured 2026-11-15), ask the user.

- **2026-11-15 Loose files are read and rewritten by TSVZ.py's rules where the two can agree**: when you read and rewrite files another program owns, read them by its rules wherever you can, and let a rewrite keep lines in their places, changing one only where every reader reads the result alike.
