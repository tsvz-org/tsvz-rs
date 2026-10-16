# tsvz-rs against TSVZ.py: tsvzbench results

Date: 2026-10-15. A ratio (×) is an implementation's time over TSVZ.py's: below 1 is faster.
Each run measured TSVZ.py 4.8, tsvz-rs 0.1.2 and tsvz-rs 0.1.0 side by side, on the same fixtures
(seed 1) on a warm page cache, with the work directory on f2fs; run 2 swapped the order of the
two tsvz-rs builds. Each tool ran its own handler for the `cli via handler` and `socket` rows.

## Commands

```bash
B=/mnt/gen5/tsvzbench-rs; mkdir -p $B/src-0.1.0
cargo build --release && cp target/release/tsvz $B/tsvz-0.1.2
git archive v0.1.0 | tar -x -C $B/src-0.1.0
(cd $B/src-0.1.0 && cargo build --release --locked) && cp $B/src-0.1.0/target/release/tsvz $B/tsvz-0.1.0
python3 ../TSVZ/tsvzbench.py --impl "py=python3 ../TSVZ/TSVZ.py" \
  --impl "rs012=$B/tsvz-0.1.2" --impl "rs010=$B/tsvz-0.1.0" \
  --suite standard --only cold,bulk,perop,maint --work-dir $B/work --json $B/run1.json | tee $B/run1.txt
# run 2: the same, with rs010 before rs012
```

## Machine and versions

- CPU: AMD Ryzen 9 9950X3D 16-Core Processor
- Kernel: 7.0.14-23-pve
- File system of the work directory: f2fs
- Python 3.13.5; TSVZ.py 4.8 @ 2026-10-14 by pan@zopyr.us; tsvz-rs 0.1.2 (`main`, commit
  0b3d729) and 0.1.0 (tag `v0.1.0`, commit b4319f9); tsvzbench 1.0

## Summary

tsvz-rs 0.1.2 is faster than TSVZ.py on every cold, bulk, per-op and maint workload in both runs,
with two exceptions: `socket --sync set`, where each request waits for its own `fdatasync`
(705-714 µs at p50 in both tools), and `socket set` in run 2 (below).

- **cold** (one-shot reads (read, len, get, verify) of 10k-1M-row stores, plain, escaped, with
  markers, checksums, gzip and multi-part): ×0.11 (geometric mean) in both runs; median ×0.18;
  every workload between ×0.02 (startup, 0.7 ms against 38.5 ms) and ×0.35 (`read 1M`, 0.78 s
  against 2.23 s). A `get` from a 1M-row store takes 57-59 ms against 1.53-1.59 s.
- **bulk** (set - and delete - batches of 10k-1M records): ×0.10 and ×0.09; every workload
  between ×0.03 and ×0.29. A million new records take 0.50-0.52 s against 2.15 s.
- **perop** (single set and get commands, the same through each tool's handler, and requests
  straight to the handler socket, alone and from 4 and 16 clients): ×0.12 and ×0.13; every
  workload but the two socket set rows between ×0.02 and ×0.36. A command takes 0.6-0.7 ms
  against 39-42 ms; the handler answers a `get` in 7 µs against 19 µs, and 16 clients mixing
  requests get 108-110k requests/s against 21-32k.
- **maint** (scrub and clear): ×0.27 in both runs; every workload between ×0.23 and ×0.33.
  `scrub multipart 100k` has no ratio: TSVZ.py refuses to scrub a multi-part store. This
  fixture's last two ordinals are consecutive, so tsvz-rs compacts the parts before the active
  one into the last of them, in place; under the default `#_rotate_# keep` the earlier parts
  stay, and the store grows from 7.2 to 13.5 MiB.
- **footprint**: 372 bytes per row read against 896 (203 against 600 for `len`); the handler
  loads a 100k-row store in 36 ms against 174 ms, holding 37 MiB against 92 MiB.

`socket set` came out ×0.78 in run 1 and ×1.01 in run 2. Within a run, the p50 of each of its
five repetitions lands near 7.3 µs or at 9.1-10.3 µs for tsvz-rs, and at 9.2-9.8 µs or near
13 µs for TSVZ.py, whatever the build or its order: run 2 caught TSVZ.py in its fast mode in
every repetition. Read it as roughly even.

0.1.0 and 0.1.2 have the same family means within 0.01 in both runs: the correctness work of
0.1.1 and 0.1.2 cost nothing measurable, but for one workload, `socket mix ×1`.

## `socket mix ×1`: 2 µs slower since 05618e0

One client sends `set k`, then `get k`, over its own keys. In both runs (and a third, per-op
only) 0.1.2 took 16-17 µs at p50 against 13-14 µs for 0.1.0, while 4 and 16 clients were a
little faster on 0.1.2 (145 µs against 152-158 µs at ×16).

`bench/mix1.py` runs that workload alone. Pinned to one CCD (`taskset -c 2-7`), its repetitions
agree to a few tenths of a µs, so every commit from 0.1.0 to 0.1.2 was built and timed with
it (8 repetitions each, interleaved). The time of the `get` after a `set`:

| Build | `get` after `set`, p50 | Step |
|---|---|---|
| v0.1.0 | 12.0 µs | |
| 69b60e2 (classic lock not lost to a close, write5) | 12.6 µs | +0.5 |
| c836b2d (one lock table per inode) | 11.7 µs | −0.9 |
| 64405fa (a store follows its last part through one kept descriptor) | 10.9 µs | −0.8 |
| 1c8c2aa | 11.2 µs | |
| **05618e0** (a waiting write goes to the store's active part) | **13.2 µs** | **+2.0** |
| v0.1.2 (0b3d729) | 13.2 µs | |

The `set` stays at about 7 µs throughout (6.9-7.4): the handler answers it once queued (`#_write_ack_#`
memory), and a read first waits for queued writes (`Engine::fresh`, `Batcher::barrier`), so the
`get` that follows pays for writing the `set`. Since 05618e0, `Writer::append_runs_at` lists the
store's directory to choose its part and lists it again once it holds the lock, to check that no
part was started meanwhile (§17.7); before, it listed once. Counted with `strace -f -c` over
2500 set/get pairs, that is one more listing (`openat`, `fstat`, two `getdents64`, `close`) and
two more `statx` a pair: 4 listings instead of 3. When the directory is older than 2 s, the
store's own checks trust its timestamp (0.1.2), and the writer's two listings are all that is
left (1c8c2aa: one).

A build of 0.1.2 without the second listing (not safe: measurement only) answers the `get` in
11.3 µs with a fresh directory and 11.6 µs with one older than 2 s, against 13.3 and 14.1 µs
for 0.1.2 and 11.3 and 11.5 µs for 1c8c2aa: that listing is the whole difference.

### The fix (0.1.3)

The writer now samples the store's directory just before its listing and, once it holds the
lock, lists the parts again only where that sample cannot vouch for the listing. Where the
directory keeps its timestamp (`parts::DirTrust`: a local kernel file system, asked through the
open directory, so that the answer and the timestamp are of one file system), the same device,
inode and change time (`ctime`: no program can set it back), in finer steps than whole seconds
and at least 2 s old when sampled, mean the same names, as the store's own checks take them;
elsewhere (mergerfs, network) the parts are listed again, as before. The store's checks and the
writer share that one rule. The handler's writer thread does not ask the file system at every
batch: only when the stamp is other than the last one asked about; where the file system does
not keep the timestamp, not again. A timestamp seen too recent is not looked at again before it may
be old enough, by the writer or the store, so a fresh directory costs no stat either. A loose
file's directory is never looked at.

`--warm 2.5` keeps the handler busy with `len` requests for 2.5 s before the timed ones, so
that the directory is as in a store nothing changed in for 2 s. Without it the directory is as
tsvzbench leaves it, about 1 s after the handler wrote its pointer file there. p50 of all the
mix requests, and of the `get` after each `set`, 12 pinned repetitions in one run per row
(2026-10-16):

| Directory | v0.1.0 | 1c8c2aa | v0.1.2 | 0.1.3 |
|---|---|---|---|---|
| Unchanged for 2 s (`--warm 2.5`) | 9.4 µs, get 10.2 | 8.8, get 9.6 | 10.7, get 11.5 | **9.3, get 10.1** |
| Changed within 2 s (as tsvzbench runs it) | 11.6 µs, get 12.1 | 11.0, get 11.5 | 12.9, get 13.5 | 12.6, get 13.1 |

A handler serving a store whose parts have not changed for 2 s writes with one listing a batch
again. tsvzbench's `socket mix ×1` row stays about where 0.1.2 has it: there the directory is
too new for its timestamp to show every change, so the parts are listed again, as they must be.

Dating the directory back (`os.utime`) instead of waiting does not show 0.1.3's steady state: a
handler that saw the timestamp too recent does not look at it again for up to 2 s, so the 50 ms
of timed requests list the parts at every check. A `--delay` instead of `--warm` leaves the CPU
idle before the timed requests, which slows every build alike (about 11 µs for each).

```bash
taskset -c 2-7 python3 bench/mix1.py --fixture $B/fixtures/plain-100000-s1-g1/plain.tsvz \
  --work-dir $B/mix --reps 12 --warm 2.5 v0.1.0=$B/tsvz-v0.1.0 1c8c2aa=$B/tsvz-1c8c2aa \
  v0.1.2=$B/tsvz-v0.1.2 0.1.3=target/release/tsvz
```

The fixture is tsvzbench's (`tsvzbench.Fixtures(B + '/fixtures', 1).get('plain', 100000)`). To
build each commit for such a comparison from `git archive`, touch the extracted files: the
archive keeps commit times, and cargo skips a rebuild it believes is fresh.

## Run to run on this machine

Against the 2026-10-14 results, `len 1M` went from 233-243 ms to 332-334 ms, `read 1M` from
697-728 ms to 777-783 ms, and the handler's peak RSS from 45 to 71 MiB, in both builds. Commit
6560e26, the one measured then, rebuilt and run alongside gives today's numbers (`len 1M`: median
336 ms, single runs down to 219 ms): the machine, not the code. Compare builds within one run,
not against an earlier file; pin (`taskset`) when µs differences matter.

## Run 1

```
tsvzbench 1.0  2026-10-15T22:33:22-0700  suite standard  seed 1
host ksxx  cpu AMD Ryzen 9 9950X3D 16-Core Processor ×32  Linux 7.0.14-23-pve  python 3.13.5  fs f2fs  warm page cache
  py           python3 /mnt/klein/work/cas/TSVZ/TSVZ.py  [TSVZ.py 4.8 @ 2026-10-14 by pan@zopyr.us]  handler: yes  startup 38.5 ms
  rs012        /mnt/gen5/tsvzbench-rs/tsvz-0.1.2  [tsvz-rs 0.1.2]  handler: yes  startup 698 µs
  rs010        /mnt/gen5/tsvzbench-rs/tsvz-0.1.0  [tsvz-rs 0.1.0]  handler: yes  startup 610 µs

[cold]
workload             | py wall  | ±  | net      | cpu      | rss       | ok | rs012 wall | ±   | net      | cpu      | rss       | ok | ×     | rs010 wall | ±   | net      | cpu      | rss       | ok | ×
---------------------+----------+----+----------+----------+-----------+----+------------+-----+----------+----------+-----------+----+-------+------------+-----+----------+----------+-----------+----+------
startup              | 38.5 ms  | 2% |          | 37.5 ms  | 34.9 MiB  | ✓  | 698 µs     | 9%  |          | 453 µs   | ≤8.8 MiB  | ✓  | ×0.02 | 610 µs     | 5%  |          | 368 µs   | ≤8.8 MiB  | ✓  | ×0.02
read 10k             | 56.1 ms  | 0% | 17.6 ms  | 55.3 ms  | 41.2 MiB  | ✓  | 6.0 ms     | 1%  | 5.3 ms   | 5.7 ms   | ≤8.8 MiB  | ✓  | ×0.11 | 5.8 ms     | 0%  | 5.2 ms   | 5.6 ms   | ≤8.8 MiB  | ✓  | ×0.10
len 10k              | 51.3 ms  | 1% | 12.8 ms  | 50.5 ms  | 39.4 MiB  | ✓  | 2.1 ms     | 6%  | 1.4 ms   | 1.9 ms   | ≤8.8 MiB  | ✓  | ×0.04 | 2.2 ms     | 5%  | 1.6 ms   | 2.0 ms   | ≤8.8 MiB  | ✓  | ×0.04
get 10k              | 50.2 ms  | 0% | 11.7 ms  | 49.4 ms  | 38.6 MiB  | ✓  | 1.8 ms     | 13% | 1.1 ms   | 1.5 ms   | ≤8.8 MiB  | ✓  | ×0.04 | 1.6 ms     | 11% | 986 µs   | 1.2 ms   | ≤8.8 MiB  | ✓  | ×0.03
read 100k            | 228.1 ms | 0% | 189.6 ms | 226.8 ms | 119.0 MiB | ✓  | 52.8 ms    | 1%  | 52.2 ms  | 52.3 ms  | 37.3 MiB  | ✓  | ×0.23 | 53.3 ms    | 0%  | 52.7 ms  | 52.7 ms  | 37.2 MiB  | ✓  | ×0.23
len 100k             | 181.3 ms | 0% | 142.8 ms | 180.3 ms | 95.7 MiB  | ✓  | 14.5 ms    | 3%  | 13.9 ms  | 14.2 ms  | 19.8 MiB  | ✓  | ×0.08 | 14.4 ms    | 1%  | 13.8 ms  | 14.1 ms  | 19.6 MiB  | ✓  | ×0.08
get 100k             | 165.2 ms | 0% | 126.7 ms | 164.0 ms | 89.3 MiB  | ✓  | 6.9 ms     | 3%  | 6.2 ms   | 6.6 ms   | ≤8.8 MiB  | ✓  | ×0.04 | 7.1 ms     | 6%  | 6.5 ms   | 6.7 ms   | ≤8.8 MiB  | ✓  | ×0.04
read 1M              | 2.23 s   | 0% | 2.19 s   | 2.23 s   | 887.3 MiB | ✓  | 777.1 ms   | 1%  | 776.4 ms | 776.5 ms | 359.2 MiB | ✓  | ×0.35 | 783.8 ms   | 1%  | 783.2 ms | 783.3 ms | 359.3 MiB | ✓  | ×0.35
len 1M               | 1.83 s   | 1% | 1.79 s   | 1.83 s   | 605.9 MiB | ✓  | 334.4 ms   | 1%  | 333.7 ms | 333.8 ms | 197.8 MiB | ✓  | ×0.18 | 332.0 ms   | 1%  | 331.4 ms | 331.4 ms | 197.5 MiB | ✓  | ×0.18
get 1M               | 1.53 s   | 0% | 1.49 s   | 1.53 s   | 556.0 MiB | ✓  | 56.6 ms    | 0%  | 55.9 ms  | 56.1 ms  | ≤8.8 MiB  | ✓  | ×0.04 | 58.6 ms    | 1%  | 58.0 ms  | 58.2 ms  | ≤8.8 MiB  | ✓  | ×0.04
read churn 100k      | 648.9 ms | 0% | 610.4 ms | 647.5 ms | 157.6 MiB | ✓  | 185.0 ms   | 4%  | 184.3 ms | 184.4 ms | 60.2 MiB  | ✓  | ×0.29 | 186.2 ms   | 1%  | 185.6 ms | 185.5 ms | 60.2 MiB  | ✓  | ×0.29
read escaped 100k    | 297.6 ms | 0% | 259.1 ms | 296.4 ms | 155.6 MiB | ✓  | 64.6 ms    | 2%  | 64.0 ms  | 64.1 ms  | 37.8 MiB  | ✓  | ×0.22 | 65.9 ms    | 0%  | 65.3 ms  | 65.4 ms  | 38.0 MiB  | ✓  | ×0.22
read markers 100k    | 243.4 ms | 0% | 204.9 ms | 242.5 ms | 106.8 MiB | ✓  | 44.1 ms    | 1%  | 43.4 ms  | 43.8 ms  | 29.7 MiB  | ✓  | ×0.18 | 44.3 ms    | 1%  | 43.6 ms  | 43.7 ms  | 29.7 MiB  | ✓  | ×0.18
read checksum 100k   | 260.8 ms | 1% | 222.2 ms | 259.7 ms | 118.9 MiB | ✓  | 52.4 ms    | 0%  | 51.7 ms  | 51.8 ms  | 37.3 MiB  | ✓  | ×0.20 | 54.2 ms    | 1%  | 53.6 ms  | 53.8 ms  | 37.3 MiB  | ✓  | ×0.21
read gz 100k         | 246.4 ms | 0% | 207.9 ms | 245.1 ms | 119.6 MiB | ✓  | 67.8 ms    | 1%  | 67.1 ms  | 67.2 ms  | 57.2 MiB  | ✓  | ×0.28 | 68.1 ms    | 1%  | 67.5 ms  | 67.5 ms  | 56.7 MiB  | ✓  | ×0.28
read multipart 100k  | 226.8 ms | 1% | 188.3 ms | 225.3 ms | 118.7 MiB | ✓  | 54.6 ms    | 2%  | 53.9 ms  | 54.0 ms  | 38.4 MiB  | ✓  | ×0.24 | 54.5 ms    | 1%  | 53.9 ms  | 53.9 ms  | 38.3 MiB  | ✓  | ×0.24
verify checksum 100k | 170.1 ms | 1% | 131.6 ms | 168.9 ms | 38.2 MiB  | ✓  | 20.1 ms    | 2%  | 19.4 ms  | 19.8 ms  | 19.7 MiB  | ✓  | ×0.12 | 20.6 ms    | 1%  | 20.0 ms  | 20.3 ms  | 19.5 MiB  | ✓  | ×0.12

[bulk]
workload       | py wall  | ±  | net      | cpu      | rss       | disk      | ok | rs012 wall | ±  | net      | cpu      | rss       | disk      | ok | ×     | rs010 wall | ±   | net      | cpu      | rss       | disk      | ok | ×
---------------+----------+----+----------+----------+-----------+-----------+----+------------+----+----------+----------+-----------+-----------+----+-------+------------+-----+----------+----------+-----------+-----------+----+------
set - new 10k  | 53.1 ms  | 1% | 14.5 ms  | 52.1 ms  | 40.6 MiB  | 737.6 KiB | ✓  | 6.0 ms     | 1% | 5.3 ms   | 5.7 ms   | ≤8.8 MiB  | 737.6 KiB | ✓  | ×0.11 | 6.1 ms     | 2%  | 5.5 ms   | 5.9 ms   | ≤8.8 MiB  | 737.6 KiB | ✓  | ×0.12
set - new 100k | 186.6 ms | 1% | 148.1 ms | 185.4 ms | 101.0 MiB | 7.2 MiB   | ✓  | 53.6 ms    | 1% | 52.9 ms  | 53.2 ms  | 60.5 MiB  | 7.2 MiB   | ✓  | ×0.29 | 53.6 ms    | 2%  | 53.0 ms  | 53.1 ms  | 60.1 MiB  | 7.2 MiB   | ✓  | ×0.29
set - new 1M   | 2.15 s   | 1% | 2.11 s   | 2.15 s   | 719.9 MiB | 72.0 MiB  | ✓  | 521.1 ms   | 1% | 520.4 ms | 520.6 ms | 581.2 MiB | 72.0 MiB  | ✓  | ×0.24 | 508.9 ms   | 1%  | 508.3 ms | 508.3 ms | 581.2 MiB | 72.0 MiB  | ✓  | ×0.24
set - add 100k | 40.7 ms  | 1% | 2.1 ms   | 39.9 ms  | 35.2 MiB  | 7.3 MiB   | ✓  | 1.3 ms     | 7% | 553 µs   | 993 µs   | ≤8.8 MiB  | 7.3 MiB   | ✓  | ×0.03 | 1.2 ms     | 0%  | 547 µs   | 932 µs   | ≤8.8 MiB  | 7.3 MiB   | ✓  | ×0.03
delete - 100k  | 45.2 ms  | 1% | 6.6 ms   | 44.4 ms  | 36.3 MiB  | 7.3 MiB   | ✓  | 1.7 ms     | 2% | 1.0 ms   | 1.3 ms   | ≤8.8 MiB  | 7.3 MiB   | ✓  | ×0.04 | 1.7 ms     | 12% | 1.0 ms   | 1.4 ms   | ≤8.8 MiB  | 7.3 MiB   | ✓  | ×0.04

[perop]
workload            | py p50   | p95      | p99      | ops/s   | rss      | ok | rs012 p50 | p95    | p99    | ops/s    | rss      | ok | ×     | rs010 p50 | p95    | p99    | ops/s    | rss      | ok | ×     | ceiling ops/s
--------------------+----------+----------+----------+---------+----------+----+-----------+--------+--------+----------+----------+----+-------+-----------+--------+--------+----------+----------+----+-------+--------------
cli set             | 39.2 ms  | 40.4 ms  | 40.8 ms  | 26/s    | 34.8 MiB | ✓  | 606 µs    | 950 µs | 1.2 ms | 1.6k/s   | ≤8.8 MiB | ✓  | ×0.02 | 602 µs    | 843 µs | 1.1 ms | 1.6k/s   | ≤8.8 MiB | ✓  | ×0.02 |
cli get             | 166.6 ms | 169.9 ms | 175.8 ms | 6/s     | 89.5 MiB | ✓  | 6.4 ms    | 6.9 ms | 7.1 ms | 157/s    | ≤8.8 MiB | ✓  | ×0.04 | 6.7 ms    | 7.3 ms | 8.0 ms | 148/s    | ≤8.8 MiB | ✓  | ×0.04 |
cli via handler set | 41.6 ms  | 43.1 ms  | 44.9 ms  | 24/s    | 34.8 MiB | ✓  | 668 µs    | 1.1 ms | 1.2 ms | 1.4k/s   | ≤8.8 MiB | ✓  | ×0.02 | 667 µs    | 1.0 ms | 1.2 ms | 1.4k/s   | ≤8.8 MiB | ✓  | ×0.02 |
cli via handler get | 41.7 ms  | 42.8 ms  | 43.5 ms  | 24/s    | 34.8 MiB | ✓  | 626 µs    | 968 µs | 1.1 ms | 1.5k/s   | ≤8.8 MiB | ✓  | ×0.02 | 629 µs    | 1.0 ms | 1.1 ms | 1.4k/s   | ≤8.8 MiB | ✓  | ×0.02 |
socket get          | 19 µs    | 21 µs    | 27 µs    | 51.0k/s |          | ✓  | 7 µs      | 9 µs   | 12 µs  | 136.7k/s |          | ✓  | ×0.36 | 7 µs      | 9 µs   | 12 µs  | 134.3k/s |          | ✓  | ×0.37 | 290.6k/s
socket set          | 12 µs    | 23 µs    | 29 µs    | 67.9k/s |          | ✓  | 9 µs      | 13 µs  | 19 µs  | 71.1k/s  |          | ✓  | ×0.78 | 9 µs      | 13 µs  | 24 µs  | 69.7k/s  |          | ✓  | ×0.77 | 267.3k/s
socket --sync set   | 714 µs   | 781 µs   | 912 µs   | 1.4k/s  |          | ✓  | 714 µs    | 782 µs | 952 µs | 1.4k/s   |          | ✓  | ×1.00 | 714 µs    | 954 µs | 1.4 ms | 1.4k/s   |          | ✓  | ×1.00 | 248.8k/s
socket mix ×1       | 50 µs    | 93 µs    | 106 µs   | 19.1k/s |          | ✓  | 17 µs     | 24 µs  | 30 µs  | 59.1k/s  |          | ✓  | ×0.33 | 14 µs     | 21 µs  | 26 µs  | 74.6k/s  |          | ✓  | ×0.28 | 255.3k/s
socket mix ×4       | 138 µs   | 293 µs   | 497 µs   | 25.2k/s |          | ✓  | 35 µs     | 64 µs  | 82 µs  | 110.6k/s |          | ✓  | ×0.25 | 38 µs     | 67 µs  | 87 µs  | 102.3k/s |          | ✓  | ×0.28 | 898.3k/s
socket mix ×16      | 574 µs   | 1.5 ms   | 2.3 ms   | 20.7k/s |          | ✓  | 145 µs    | 309 µs | 462 µs | 107.7k/s |          | ✓  | ×0.25 | 158 µs    | 334 µs | 505 µs | 103.4k/s |          | ✓  | ×0.27 | 1.84M/s

[maint]
workload             | py wall  | ±  | net      | cpu      | rss       | disk             | ok      | rs012 wall | ±  | net      | cpu      | rss       | disk             | ok | ×     | rs010 wall | ±  | net      | cpu      | rss       | disk             | ok | ×
---------------------+----------+----+----------+----------+-----------+------------------+---------+------------+----+----------+----------+-----------+------------------+----+-------+------------+----+----------+----------+-----------+------------------+----+------
scrub churn 100k     | 706.5 ms | 3% | 667.9 ms | 702.0 ms | 170.8 MiB | 24.0 MiB→7.2 MiB | ✓       | 231.0 ms   | 4% | 230.3 ms | 226.2 ms | 106.7 MiB | 24.0 MiB→7.2 MiB | ✓  | ×0.33 | 233.0 ms   | 1% | 232.4 ms | 228.3 ms | 106.5 MiB | 24.0 MiB→7.2 MiB | ✓  | ×0.33
scrub markers 100k   | 266.0 ms | 1% | 227.5 ms | 261.8 ms | 107.1 MiB | 4.4 MiB→7.4 MiB  | ✓       | 68.8 ms    | 2% | 68.1 ms  | 65.7 ms  | 68.7 MiB  | 4.4 MiB→7.4 MiB  | ✓  | ×0.26 | 70.1 ms    | 3% | 69.5 ms  | 66.6 ms  | 68.6 MiB  | 4.4 MiB→7.4 MiB  | ✓  | ×0.26
clear 100k           | 171.0 ms | 1% | 132.4 ms | 166.6 ms | 93.3 MiB  | 7.2 MiB→20 B     | ✓       | 40.9 ms    | 3% | 40.2 ms  | 38.9 ms  | 43.5 MiB  | 7.2 MiB→20 B     | ✓  | ×0.24 | 39.5 ms    | 0% | 38.8 ms  | 37.2 ms  | 43.3 MiB  | 7.2 MiB→20 B     | ✓  | ×0.23
scrub multipart 100k | 165.4 ms | 1% | 126.8 ms | 163.9 ms | 87.1 MiB  | 7.2 MiB→7.2 MiB  | refused | 234.8 ms   | 1% | 234.1 ms | 229.1 ms | 157.1 MiB | 7.2 MiB→13.5 MiB | ✓  | —     | 237.5 ms   | 1% | 236.9 ms | 231.8 ms | 157.3 MiB | 7.2 MiB→13.5 MiB | ✓  | —

[footprint]
  py           memory/row (read) 896 B; memory/row (len) 600 B; disk/record 75 B; handler load 173.8 ms, rss after load 92.4 MiB, peak rss 96.3 MiB, cpu 6.30 s
  rs012        memory/row (read) 372 B; memory/row (len) 203 B; disk/record 75 B; handler load 35.8 ms, rss after load 36.8 MiB, peak rss 70.6 MiB, cpu 1.89 s
  rs010        memory/row (read) 373 B; memory/row (len) 203 B; disk/record 75 B; handler load 40.9 ms, rss after load 36.8 MiB, peak rss 71.0 MiB, cpu 1.96 s

[summary] ratio = time against py, geometric mean per family (lower is faster)
  py           cold ×1.00  bulk ×1.00  perop ×1.00  maint ×1.00   ✗ 0  ! 0  ⏱ 0
  rs012        cold ×0.11  bulk ×0.10  perop ×0.12  maint ×0.27   ✗ 0  ! 0  ⏱ 0
  rs010        cold ×0.11  bulk ×0.10  perop ×0.12  maint ×0.27   ✗ 0  ! 0  ⏱ 0
```

## Run 2

```
tsvzbench 1.0  2026-10-15T22:38:01-0700  suite standard  seed 1
host ksxx  cpu AMD Ryzen 9 9950X3D 16-Core Processor ×32  Linux 7.0.14-23-pve  python 3.13.5  fs f2fs  warm page cache
  py           python3 /mnt/klein/work/cas/TSVZ/TSVZ.py  [TSVZ.py 4.8 @ 2026-10-14 by pan@zopyr.us]  handler: yes  startup 38.5 ms
  rs010        /mnt/gen5/tsvzbench-rs/tsvz-0.1.0  [tsvz-rs 0.1.0]  handler: yes  startup 821 µs
  rs012        /mnt/gen5/tsvzbench-rs/tsvz-0.1.2  [tsvz-rs 0.1.2]  handler: yes  startup 675 µs

[cold]
workload             | py wall  | ±  | net      | cpu      | rss       | ok | rs010 wall | ±   | net      | cpu      | rss       | ok | ×     | rs012 wall | ±   | net      | cpu      | rss       | ok | ×
---------------------+----------+----+----------+----------+-----------+----+------------+-----+----------+----------+-----------+----+-------+------------+-----+----------+----------+-----------+----+------
startup              | 38.5 ms  | 2% |          | 37.7 ms  | 35.0 MiB  | ✓  | 821 µs     | 17% |          | 583 µs   | ≤8.8 MiB  | ✓  | ×0.02 | 675 µs     | 15% |          | 429 µs   | ≤8.8 MiB  | ✓  | ×0.02
read 10k             | 56.7 ms  | 1% | 18.2 ms  | 55.6 ms  | 41.6 MiB  | ✓  | 6.2 ms     | 3%  | 5.4 ms   | 5.9 ms   | ≤8.7 MiB  | ✓  | ×0.11 | 6.1 ms     | 7%  | 5.5 ms   | 5.9 ms   | ≤8.8 MiB  | ✓  | ×0.11
len 10k              | 51.9 ms  | 1% | 13.5 ms  | 51.0 ms  | 39.0 MiB  | ✓  | 2.5 ms     | 12% | 1.7 ms   | 2.2 ms   | ≤8.8 MiB  | ✓  | ×0.05 | 2.2 ms     | 2%  | 1.5 ms   | 1.9 ms   | ≤8.8 MiB  | ✓  | ×0.04
get 10k              | 50.4 ms  | 1% | 11.9 ms  | 49.4 ms  | 38.5 MiB  | ✓  | 1.7 ms     | 7%  | 915 µs   | 1.5 ms   | ≤8.8 MiB  | ✓  | ×0.03 | 1.9 ms     | 8%  | 1.2 ms   | 1.5 ms   | ≤8.8 MiB  | ✓  | ×0.04
read 100k            | 231.2 ms | 0% | 192.7 ms | 230.0 ms | 119.4 MiB | ✓  | 54.1 ms    | 2%  | 53.3 ms  | 53.7 ms  | 37.3 MiB  | ✓  | ×0.23 | 54.8 ms    | 1%  | 54.1 ms  | 54.2 ms  | 37.5 MiB  | ✓  | ×0.24
len 100k             | 183.3 ms | 0% | 144.8 ms | 182.2 ms | 95.6 MiB  | ✓  | 15.0 ms    | 2%  | 14.1 ms  | 14.7 ms  | 19.7 MiB  | ✓  | ×0.08 | 14.7 ms    | 2%  | 14.0 ms  | 14.5 ms  | 19.8 MiB  | ✓  | ×0.08
get 100k             | 165.5 ms | 0% | 127.0 ms | 164.4 ms | 89.3 MiB  | ✓  | 7.4 ms     | 3%  | 6.6 ms   | 7.0 ms   | ≤8.8 MiB  | ✓  | ×0.04 | 7.1 ms     | 2%  | 6.4 ms   | 6.8 ms   | ≤8.8 MiB  | ✓  | ×0.04
read 1M              | 2.25 s   | 0% | 2.22 s   | 2.25 s   | 887.4 MiB | ✓  | 782.7 ms   | 1%  | 781.8 ms | 782.0 ms | 359.2 MiB | ✓  | ×0.35 | 781.6 ms   | 1%  | 780.9 ms | 780.9 ms | 359.0 MiB | ✓  | ×0.35
len 1M               | 1.78 s   | 0% | 1.74 s   | 1.78 s   | 606.4 MiB | ✓  | 333.0 ms   | 1%  | 332.1 ms | 332.4 ms | 197.4 MiB | ✓  | ×0.19 | 332.7 ms   | 1%  | 332.1 ms | 332.2 ms | 197.5 MiB | ✓  | ×0.19
get 1M               | 1.59 s   | 1% | 1.56 s   | 1.59 s   | 556.0 MiB | ✓  | 59.7 ms    | 0%  | 58.8 ms  | 59.2 ms  | ≤8.8 MiB  | ✓  | ×0.04 | 58.5 ms    | 1%  | 57.9 ms  | 58.1 ms  | ≤8.8 MiB  | ✓  | ×0.04
read churn 100k      | 649.7 ms | 1% | 611.2 ms | 648.4 ms | 157.8 MiB | ✓  | 184.7 ms   | 1%  | 183.9 ms | 184.1 ms | 60.0 MiB  | ✓  | ×0.28 | 185.1 ms   | 0%  | 184.5 ms | 184.5 ms | 60.4 MiB  | ✓  | ×0.28
read escaped 100k    | 296.9 ms | 0% | 258.5 ms | 295.7 ms | 155.8 MiB | ✓  | 63.5 ms    | 1%  | 62.7 ms  | 62.9 ms  | 37.9 MiB  | ✓  | ×0.21 | 64.8 ms    | 1%  | 64.1 ms  | 64.2 ms  | 37.8 MiB  | ✓  | ×0.22
read markers 100k    | 244.9 ms | 0% | 206.4 ms | 243.5 ms | 106.9 MiB | ✓  | 43.9 ms    | 2%  | 43.0 ms  | 43.3 ms  | 29.5 MiB  | ✓  | ×0.18 | 44.5 ms    | 1%  | 43.9 ms  | 44.2 ms  | 29.8 MiB  | ✓  | ×0.18
read checksum 100k   | 261.3 ms | 0% | 222.8 ms | 260.1 ms | 119.3 MiB | ✓  | 52.8 ms    | 1%  | 52.0 ms  | 52.4 ms  | 37.3 MiB  | ✓  | ×0.20 | 53.1 ms    | 1%  | 52.4 ms  | 52.6 ms  | 37.5 MiB  | ✓  | ×0.20
read gz 100k         | 270.1 ms | 9% | 231.6 ms | 269.1 ms | 117.5 MiB | ✓  | 69.8 ms    | 3%  | 69.0 ms  | 69.4 ms  | 56.7 MiB  | ✓  | ×0.26 | 71.1 ms    | 3%  | 70.4 ms  | 70.7 ms  | 57.0 MiB  | ✓  | ×0.26
read multipart 100k  | 225.9 ms | 0% | 187.4 ms | 224.7 ms | 118.6 MiB | ✓  | 54.8 ms    | 1%  | 54.0 ms  | 54.2 ms  | 38.3 MiB  | ✓  | ×0.24 | 53.9 ms    | 1%  | 53.2 ms  | 53.4 ms  | 38.2 MiB  | ✓  | ×0.24
verify checksum 100k | 168.1 ms | 1% | 129.6 ms | 167.1 ms | 38.7 MiB  | ✓  | 19.8 ms    | 1%  | 19.0 ms  | 19.4 ms  | 19.7 MiB  | ✓  | ×0.12 | 19.7 ms    | 2%  | 19.1 ms  | 19.3 ms  | 19.7 MiB  | ✓  | ×0.12

[bulk]
workload       | py wall  | ±  | net      | cpu      | rss       | disk      | ok | rs010 wall | ±  | net      | cpu      | rss       | disk      | ok | ×     | rs012 wall | ±  | net      | cpu      | rss       | disk      | ok | ×
---------------+----------+----+----------+----------+-----------+-----------+----+------------+----+----------+----------+-----------+-----------+----+-------+------------+----+----------+----------+-----------+-----------+----+------
set - new 10k  | 53.1 ms  | 1% | 14.6 ms  | 52.1 ms  | 40.5 MiB  | 737.6 KiB | ✓  | 5.7 ms     | 3% | 4.9 ms   | 5.4 ms   | ≤8.8 MiB  | 737.6 KiB | ✓  | ×0.11 | 5.6 ms     | 0% | 4.9 ms   | 5.3 ms   | ≤8.8 MiB  | 737.6 KiB | ✓  | ×0.10
set - new 100k | 185.5 ms | 1% | 147.0 ms | 184.4 ms | 101.3 MiB | 7.2 MiB   | ✓  | 53.1 ms    | 1% | 52.3 ms  | 52.5 ms  | 60.4 MiB  | 7.2 MiB   | ✓  | ×0.29 | 52.6 ms    | 1% | 51.9 ms  | 52.2 ms  | 60.3 MiB  | 7.2 MiB   | ✓  | ×0.28
set - new 1M   | 2.16 s   | 0% | 2.12 s   | 2.15 s   | 719.8 MiB | 72.0 MiB  | ✓  | 507.8 ms   | 1% | 507.0 ms | 507.1 ms | 581.1 MiB | 72.0 MiB  | ✓  | ×0.24 | 504.5 ms   | 1% | 503.8 ms | 503.9 ms | 581.0 MiB | 72.0 MiB  | ✓  | ×0.23
set - add 100k | 41.0 ms  | 1% | 2.5 ms   | 40.0 ms  | 34.9 MiB  | 7.3 MiB   | ✓  | 1.2 ms     | 3% | 392 µs   | 958 µs   | ≤8.8 MiB  | 7.3 MiB   | ✓  | ×0.03 | 1.3 ms     | 5% | 612 µs   | 947 µs   | ≤8.8 MiB  | 7.3 MiB   | ✓  | ×0.03
delete - 100k  | 45.8 ms  | 3% | 7.3 ms   | 44.7 ms  | 36.0 MiB  | 7.3 MiB   | ✓  | 1.9 ms     | 1% | 1.1 ms   | 1.6 ms   | ≤8.9 MiB  | 7.3 MiB   | ✓  | ×0.04 | 1.5 ms     | 3% | 833 µs   | 1.3 ms   | ≤8.8 MiB  | 7.3 MiB   | ✓  | ×0.03

[perop]
workload            | py p50   | p95      | p99      | ops/s   | rss      | ok | rs010 p50 | p95    | p99    | ops/s    | rss      | ok | ×     | rs012 p50 | p95    | p99    | ops/s    | rss      | ok | ×     | ceiling ops/s
--------------------+----------+----------+----------+---------+----------+----+-----------+--------+--------+----------+----------+----+-------+-----------+--------+--------+----------+----------+----+-------+--------------
cli set             | 39.4 ms  | 40.9 ms  | 42.6 ms  | 25/s    | 34.8 MiB | ✓  | 602 µs    | 1.1 ms | 1.2 ms | 1.5k/s   | ≤8.8 MiB | ✓  | ×0.02 | 613 µs    | 1.0 ms | 1.1 ms | 1.6k/s   | ≤8.8 MiB | ✓  | ×0.02 |
cli get             | 166.8 ms | 169.8 ms | 173.9 ms | 6/s     | 89.5 MiB | ✓  | 6.7 ms    | 7.3 ms | 7.5 ms | 148/s    | ≤8.8 MiB | ✓  | ×0.04 | 6.6 ms    | 7.4 ms | 7.9 ms | 149/s    | ≤8.8 MiB | ✓  | ×0.04 |
cli via handler set | 41.7 ms  | 43.0 ms  | 44.5 ms  | 24/s    | 34.9 MiB | ✓  | 670 µs    | 1.0 ms | 1.3 ms | 1.4k/s   | ≤8.8 MiB | ✓  | ×0.02 | 675 µs    | 1.1 ms | 1.3 ms | 1.4k/s   | ≤8.8 MiB | ✓  | ×0.02 |
cli via handler get | 41.6 ms  | 42.7 ms  | 43.6 ms  | 24/s    | 35.0 MiB | ✓  | 628 µs    | 904 µs | 1.0 ms | 1.5k/s   | ≤8.8 MiB | ✓  | ×0.02 | 636 µs    | 947 µs | 1.0 ms | 1.5k/s   | ≤8.8 MiB | ✓  | ×0.02 |
socket get          | 19 µs    | 21 µs    | 26 µs    | 51.6k/s |          | ✓  | 7 µs      | 9 µs   | 11 µs  | 137.2k/s |          | ✓  | ×0.36 | 7 µs      | 9 µs   | 12 µs  | 133.3k/s |          | ✓  | ×0.36 | 289.6k/s
socket set          | 9 µs     | 14 µs    | 21 µs    | 95.9k/s |          | ✓  | 9 µs      | 12 µs  | 27 µs  | 96.2k/s  |          | ✓  | ×0.96 | 10 µs     | 13 µs  | 23 µs  | 70.4k/s  |          | ✓  | ×1.01 | 251.1k/s
socket --sync set   | 705 µs   | 772 µs   | 897 µs   | 1.4k/s  |          | ✓  | 705 µs    | 773 µs | 945 µs | 1.4k/s   |          | ✓  | ×1.00 | 705 µs    | 772 µs | 951 µs | 1.4k/s   |          | ✓  | ×1.00 | 255.2k/s
socket mix ×1       | 45 µs    | 81 µs    | 101 µs   | 26.6k/s |          | ✓  | 13 µs     | 21 µs  | 27 µs  | 76.6k/s  |          | ✓  | ×0.28 | 16 µs     | 23 µs  | 30 µs  | 60.9k/s  |          | ✓  | ×0.35 | 263.9k/s
socket mix ×4       | 132 µs   | 266 µs   | 472 µs   | 30.2k/s |          | ✓  | 37 µs     | 66 µs  | 82 µs  | 106.5k/s |          | ✓  | ×0.28 | 33 µs     | 59 µs  | 77 µs  | 115.7k/s |          | ✓  | ×0.25 | 955.9k/s
socket mix ×16      | 411 µs   | 1.2 ms   | 1.7 ms   | 32.3k/s |          | ✓  | 152 µs    | 339 µs | 521 µs | 101.0k/s |          | ✓  | ×0.37 | 145 µs    | 308 µs | 460 µs | 109.8k/s |          | ✓  | ×0.35 | 1.75M/s

[maint]
workload             | py wall  | ±  | net      | cpu      | rss       | disk             | ok      | rs010 wall | ±  | net      | cpu      | rss       | disk             | ok | ×     | rs012 wall | ±  | net      | cpu      | rss       | disk             | ok | ×
---------------------+----------+----+----------+----------+-----------+------------------+---------+------------+----+----------+----------+-----------+------------------+----+-------+------------+----+----------+----------+-----------+------------------+----+------
scrub churn 100k     | 725.5 ms | 2% | 687.0 ms | 721.0 ms | 170.7 MiB | 24.0 MiB→7.2 MiB | ✓       | 234.9 ms   | 1% | 234.1 ms | 230.1 ms | 106.7 MiB | 24.0 MiB→7.2 MiB | ✓  | ×0.32 | 235.9 ms   | 2% | 235.2 ms | 230.9 ms | 106.7 MiB | 24.0 MiB→7.2 MiB | ✓  | ×0.33
scrub markers 100k   | 268.9 ms | 0% | 230.5 ms | 264.9 ms | 106.9 MiB | 4.4 MiB→7.4 MiB  | ✓       | 68.2 ms    | 1% | 67.4 ms  | 65.0 ms  | 68.6 MiB  | 4.4 MiB→7.4 MiB  | ✓  | ×0.25 | 69.6 ms    | 1% | 69.0 ms  | 65.4 ms  | 68.7 MiB  | 4.4 MiB→7.4 MiB  | ✓  | ×0.26
clear 100k           | 171.6 ms | 0% | 133.1 ms | 167.4 ms | 93.2 MiB  | 7.2 MiB→20 B     | ✓       | 40.2 ms    | 1% | 39.3 ms  | 38.0 ms  | 43.4 MiB  | 7.2 MiB→20 B     | ✓  | ×0.23 | 39.9 ms    | 1% | 39.2 ms  | 38.0 ms  | 43.6 MiB  | 7.2 MiB→20 B     | ✓  | ×0.23
scrub multipart 100k | 169.1 ms | 0% | 130.7 ms | 168.0 ms | 86.9 MiB  | 7.2 MiB→7.2 MiB  | refused | 239.4 ms   | 1% | 238.6 ms | 233.7 ms | 157.3 MiB | 7.2 MiB→13.5 MiB | ✓  | —     | 245.7 ms   | 1% | 245.0 ms | 239.9 ms | 157.2 MiB | 7.2 MiB→13.5 MiB | ✓  | —

[footprint]
  py           memory/row (read) 896 B; memory/row (len) 601 B; disk/record 75 B; handler load 173.8 ms, rss after load 92.4 MiB, peak rss 96.5 MiB, cpu 4.86 s
  rs010        memory/row (read) 373 B; memory/row (len) 203 B; disk/record 75 B; handler load 35.9 ms, rss after load 36.8 MiB, peak rss 70.9 MiB, cpu 1.88 s
  rs012        memory/row (read) 372 B; memory/row (len) 203 B; disk/record 75 B; handler load 35.9 ms, rss after load 36.9 MiB, peak rss 70.6 MiB, cpu 1.85 s

[summary] ratio = time against py, geometric mean per family (lower is faster)
  py           cold ×1.00  bulk ×1.00  perop ×1.00  maint ×1.00   ✗ 0  ! 0  ⏱ 0
  rs010        cold ×0.12  bulk ×0.10  perop ×0.13  maint ×0.27   ✗ 0  ! 0  ⏱ 0
  rs012        cold ×0.11  bulk ×0.09  perop ×0.13  maint ×0.27   ✗ 0  ! 0  ⏱ 0
```
