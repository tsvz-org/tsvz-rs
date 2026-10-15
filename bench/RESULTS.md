# tsvz-rs against TSVZ.py: tsvzbench results

Date: 2026-10-14. A ratio (×) is tsvz-rs's time over TSVZ.py's: below 1 is faster. Both runs
used the same fixtures (seed 1) on a warm page cache, with the work directory on f2fs. Both
tools ran their own handler for the `cli via handler` and `socket` rows.

## Commands

```bash
cargo build --release
mkdir -p /mnt/gen5/tsvzbench-rs
for run in 1 2; do
  python3 ../TSVZ/tsvzbench.py --impl "py=python3 ../TSVZ/TSVZ.py" --impl "rs=target/release/tsvz" \
    --suite standard --only cold,bulk,perop,maint --work-dir /mnt/gen5/tsvzbench-rs \
    --json /mnt/gen5/tsvzbench-rs/run$run.json | tee /mnt/gen5/tsvzbench-rs/run$run.txt
done
```

## Machine and versions

- CPU: AMD Ryzen 9 9950X3D 16-Core Processor
- Kernel: 7.0.14-23-pve
- File system of the work directory: f2fs
- Python 3.13.5; TSVZ.py 4.8 @ 2026-10-14 by pan@zopyr.us; tsvz-rs 0.1.0 (branch `prerelease`, commit 6560e26)

## Summary

tsvz-rs is faster than TSVZ.py on every cold, bulk, per-op and maint workload, in both runs, with
one tie: `socket --sync set`, where each request waits for its own `fdatasync`, takes the
device's flush latency in both tools (712-718 µs at p50).

- **cold** (one-shot reads (read, len, get, verify) of 10k-1M-row stores, plain, escaped, with markers, checksums, gzip and multi-part): median ratio ×0.13 in both runs; every workload between ×0.02 and ×0.32.
- **bulk** (set - and delete - batches of 10k-1M records): median ratio ×0.11 in both runs; every workload between ×0.04 and ×0.28.
- **perop** (single set and get commands, the same through each tool's handler, and requests straight to the handler socket, alone and from 4 and 16 clients): median ratio ×0.30 and ×0.29; every workload between ×0.02 and ×0.91, except `socket --sync set` at ×1.00 (see above). A command through the handler takes 0.6-0.7 ms against 42 ms; the handler answers a `get` in 7 µs against 19 µs, and 16 clients mixing requests get 102k requests/s against 21-24k.
- **maint** (scrub and clear): median ratio ×0.26 in both runs; every workload between ×0.23 and ×0.33.

`socket set` is the narrowest margin (×0.91 and ×0.82; 9 µs at p50 against 10-11 µs). Before each
write the handler now checks that no other writer changed the store since it last read it, so
that `#_write_ack_#` and `#_member_size_#` are decided on the markers in force (it was ×0.41 when
it decided on the markers as of the last read). The 16-client mix also gets less than on
2026-10-13 (102k against 131-136k requests/s); measured alone, that check took it from 131k to
115k.

`scrub multipart 100k` has no ratio: TSVZ.py refuses to scrub a multi-part store. tsvz-rs
compacts the parts before the active one into the last of them; under the default
`#_rotate_# keep` the earlier parts stay, so the store grows (7.2 MiB → 13.5 MiB);
`#_rotate_# rename` or `delete` moves them out of the store.

The handler loads a 100k-row store in 36 ms against 175-182 ms, holding 37 MiB against 92 MiB.

## Run 1

```
tsvzbench: probing py
tsvzbench: probing rs
tsvzbench 1.0  2026-10-14T17:52:32-0700  suite standard  seed 1
host ksxx  cpu AMD Ryzen 9 9950X3D 16-Core Processor ×32  Linux 7.0.14-23-pve  python 3.13.5  fs f2fs  warm page cache
  py           python3 /mnt/klein/work/cas/TSVZ/TSVZ.py  [TSVZ.py 4.8 @ 2026-10-14 by pan@zopyr.us]  handler: yes  startup 38.3 ms
  rs           /mnt/gen5/tsvzbench-rs/tsvz-release-candidate  [tsvz-rs 0.1.0]  handler: yes  startup 880 µs

[cold]
workload             | py wall  | ±  | net      | cpu      | rss       | ok | rs wall  | ±   | net      | cpu      | rss       | ok | ×
---------------------+----------+----+----------+----------+-----------+----+----------+-----+----------+----------+-----------+----+------
startup              | 38.3 ms  | 1% |          | 37.4 ms  | 34.2 MiB  | ✓  | 880 µs   | 17% |          | 614 µs   | ≤8.8 MiB  | ✓  | ×0.02
read 10k             | 56.8 ms  | 0% | 18.5 ms  | 55.9 ms  | 41.5 MiB  | ✓  | 5.9 ms   | 3%  | 5.0 ms   | 5.6 ms   | ≤8.8 MiB  | ✓  | ×0.10
len 10k              | 51.1 ms  | 1% | 12.9 ms  | 50.4 ms  | 38.8 MiB  | ✓  | 2.5 ms   | 6%  | 1.6 ms   | 2.2 ms   | ≤8.8 MiB  | ✓  | ×0.05
get 10k              | 50.6 ms  | 0% | 12.4 ms  | 49.7 ms  | 38.8 MiB  | ✓  | 1.6 ms   | 6%  | 762 µs   | 1.4 ms   | ≤8.8 MiB  | ✓  | ×0.03
read 100k            | 231.8 ms | 1% | 193.6 ms | 230.7 ms | 119.0 MiB | ✓  | 51.3 ms  | 1%  | 50.4 ms  | 51.0 ms  | 37.2 MiB  | ✓  | ×0.22
len 100k             | 178.3 ms | 1% | 140.0 ms | 177.3 ms | 95.5 MiB  | ✓  | 14.9 ms  | 0%  | 14.0 ms  | 14.7 ms  | 19.5 MiB  | ✓  | ×0.08
get 100k             | 167.8 ms | 0% | 129.5 ms | 166.9 ms | 89.4 MiB  | ✓  | 7.0 ms   | 0%  | 6.1 ms   | 6.7 ms   | ≤8.8 MiB  | ✓  | ×0.04
read 1M              | 2.25 s   | 1% | 2.21 s   | 2.25 s   | 886.8 MiB | ✓  | 727.9 ms | 3%  | 727.1 ms | 727.5 ms | 358.9 MiB | ✓  | ×0.32
len 1M               | 1.83 s   | 1% | 1.79 s   | 1.83 s   | 605.9 MiB | ✓  | 243.4 ms | 6%  | 242.6 ms | 243.0 ms | 197.4 MiB | ✓  | ×0.13
get 1M               | 1.60 s   | 0% | 1.57 s   | 1.60 s   | 555.8 MiB | ✓  | 58.2 ms  | 2%  | 57.3 ms  | 57.7 ms  | ≤8.8 MiB  | ✓  | ×0.04
read churn 100k      | 663.7 ms | 0% | 625.5 ms | 662.5 ms | 157.5 MiB | ✓  | 187.7 ms | 7%  | 186.8 ms | 187.1 ms | 60.1 MiB  | ✓  | ×0.28
read escaped 100k    | 305.1 ms | 0% | 266.8 ms | 303.8 ms | 155.5 MiB | ✓  | 66.1 ms  | 3%  | 65.3 ms  | 65.7 ms  | 38.0 MiB  | ✓  | ×0.22
read markers 100k    | 249.3 ms | 1% | 211.1 ms | 248.2 ms | 106.8 MiB | ✓  | 45.3 ms  | 1%  | 44.4 ms  | 44.6 ms  | 29.7 MiB  | ✓  | ×0.18
read checksum 100k   | 265.2 ms | 1% | 227.0 ms | 263.9 ms | 119.2 MiB | ✓  | 52.7 ms  | 1%  | 51.8 ms  | 52.2 ms  | 37.4 MiB  | ✓  | ×0.20
read gz 100k         | 252.7 ms | 1% | 214.4 ms | 251.4 ms | 119.4 MiB | ✓  | 69.6 ms  | 1%  | 68.7 ms  | 69.0 ms  | 56.9 MiB  | ✓  | ×0.28
read multipart 100k  | 235.4 ms | 1% | 197.1 ms | 234.1 ms | 118.9 MiB | ✓  | 56.8 ms  | 2%  | 55.9 ms  | 56.2 ms  | 38.0 MiB  | ✓  | ×0.24
verify checksum 100k | 170.5 ms | 1% | 132.2 ms | 169.5 ms | 38.7 MiB  | ✓  | 20.3 ms  | 3%  | 19.4 ms  | 19.7 ms  | 19.5 MiB  | ✓  | ×0.12

[bulk]
workload       | py wall  | ±  | net      | cpu      | rss       | disk      | ok | rs wall  | ±   | net      | cpu      | rss       | disk      | ok | ×
---------------+----------+----+----------+----------+-----------+-----------+----+----------+-----+----------+----------+-----------+-----------+----+------
set - new 10k  | 53.8 ms  | 1% | 15.6 ms  | 52.7 ms  | 40.5 MiB  | 737.6 KiB | ✓  | 6.0 ms   | 6%  | 5.1 ms   | 5.7 ms   | ≤8.8 MiB  | 737.6 KiB | ✓  | ×0.11
set - new 100k | 189.0 ms | 1% | 150.8 ms | 187.8 ms | 101.1 MiB | 7.2 MiB   | ✓  | 53.2 ms  | 2%  | 52.3 ms  | 52.7 ms  | 60.1 MiB  | 7.2 MiB   | ✓  | ×0.28
set - new 1M   | 2.17 s   | 1% | 2.13 s   | 2.17 s   | 719.7 MiB | 72.0 MiB  | ✓  | 511.3 ms | 0%  | 510.5 ms | 510.7 ms | 580.9 MiB | 72.0 MiB  | ✓  | ×0.24
set - add 100k | 41.0 ms  | 0% | 2.7 ms   | 40.1 ms  | 34.8 MiB  | 7.3 MiB   | ✓  | 1.5 ms   | 17% | 612 µs   | 1.3 ms   | ≤8.8 MiB  | 7.3 MiB   | ✓  | ×0.04
delete - 100k  | 46.5 ms  | 1% | 8.2 ms   | 45.4 ms  | 36.0 MiB  | 7.3 MiB   | ✓  | 1.8 ms   | 8%  | 881 µs   | 1.4 ms   | ≤8.8 MiB  | 7.3 MiB   | ✓  | ×0.04

[perop]
workload            | py p50   | p95      | p99      | ops/s   | rss      | ok | rs p50 | p95    | p99    | ops/s    | rss      | ok | ×     | ceiling ops/s
--------------------+----------+----------+----------+---------+----------+----+--------+--------+--------+----------+----------+----+-------+--------------
cli set             | 39.8 ms  | 41.6 ms  | 43.9 ms  | 25/s    | 34.8 MiB | ✓  | 622 µs | 924 µs | 1.1 ms | 1.5k/s   | ≤8.8 MiB | ✓  | ×0.02 |
cli get             | 169.3 ms | 171.7 ms | 173.7 ms | 6/s     | 89.5 MiB | ✓  | 6.6 ms | 7.2 ms | 7.4 ms | 149/s    | ≤8.8 MiB | ✓  | ×0.04 |
cli via handler set | 41.6 ms  | 43.5 ms  | 44.0 ms  | 24/s    | 35.0 MiB | ✓  | 695 µs | 1.1 ms | 1.3 ms | 1.3k/s   | ≤8.8 MiB | ✓  | ×0.02 |
cli via handler get | 41.7 ms  | 43.5 ms  | 45.4 ms  | 24/s    | 35.0 MiB | ✓  | 641 µs | 934 µs | 1.1 ms | 1.5k/s   | ≤8.8 MiB | ✓  | ×0.02 |
socket get          | 19 µs    | 21 µs    | 27 µs    | 50.3k/s |          | ✓  | 7 µs   | 9 µs   | 12 µs  | 138.0k/s |          | ✓  | ×0.36 | 288.1k/s
socket set          | 11 µs    | 22 µs    | 28 µs    | 68.5k/s |          | ✓  | 9 µs   | 14 µs  | 31 µs  | 95.4k/s  |          | ✓  | ×0.82 | 233.8k/s
socket --sync set   | 718 µs   | 787 µs   | 968 µs   | 1.3k/s  |          | ✓  | 718 µs | 783 µs | 913 µs | 1.4k/s   |          | ✓  | ×1.00 | 244.0k/s
socket mix ×1       | 46 µs    | 81 µs    | 91 µs    | 27.0k/s |          | ✓  | 15 µs  | 21 µs  | 27 µs  | 71.4k/s  |          | ✓  | ×0.33 | 239.7k/s
socket mix ×4       | 125 µs   | 239 µs   | 464 µs   | 30.5k/s |          | ✓  | 37 µs  | 68 µs  | 86 µs  | 108.7k/s |          | ✓  | ×0.30 | 933.2k/s
socket mix ×16      | 521 µs   | 1.4 ms   | 2.1 ms   | 23.6k/s |          | ✓  | 158 µs | 340 µs | 554 µs | 101.8k/s |          | ✓  | ×0.30 | 1.81M/s

[maint]
workload             | py wall  | ±  | net      | cpu      | rss       | disk             | ok      | rs wall  | ±   | net      | cpu      | rss       | disk             | ok | ×
---------------------+----------+----+----------+----------+-----------+------------------+---------+----------+-----+----------+----------+-----------+------------------+----+------
scrub churn 100k     | 717.6 ms | 1% | 679.3 ms | 713.3 ms | 170.4 MiB | 24.0 MiB→7.2 MiB | ✓       | 195.5 ms | 11% | 194.6 ms | 190.4 ms | 106.7 MiB | 24.0 MiB→7.2 MiB | ✓  | ×0.27
scrub markers 100k   | 273.0 ms | 0% | 234.7 ms | 267.8 ms | 106.9 MiB | 4.4 MiB→7.4 MiB  | ✓       | 70.3 ms  | 1%  | 69.5 ms  | 67.3 ms  | 68.7 MiB  | 4.4 MiB→7.4 MiB  | ✓  | ×0.26
clear 100k           | 173.5 ms | 0% | 135.2 ms | 169.4 ms | 93.1 MiB  | 7.2 MiB→20 B     | ✓       | 39.8 ms  | 0%  | 38.9 ms  | 37.9 ms  | 43.3 MiB  | 7.2 MiB→20 B     | ✓  | ×0.23
scrub multipart 100k | 168.2 ms | 0% | 129.9 ms | 167.1 ms | 87.2 MiB  | 7.2 MiB→7.2 MiB  | refused | 250.8 ms | 0%  | 249.9 ms | 244.4 ms | 157.0 MiB | 7.2 MiB→13.5 MiB | ✓  | —

[footprint]
  py           memory/row (read) 895 B; memory/row (len) 601 B; disk/record 75 B; handler load 175.0 ms, rss after load 92.3 MiB, peak rss 96.3 MiB, cpu 5.54 s
  rs           memory/row (read) 372 B; memory/row (len) 203 B; disk/record 75 B; handler load 35.9 ms, rss after load 36.9 MiB, peak rss 44.8 MiB, cpu 1.91 s

[summary] ratio = time against py, geometric mean per family (lower is faster)
  py           cold ×1.00  bulk ×1.00  perop ×1.00  maint ×1.00   ✗ 0  ! 0  ⏱ 0
  rs           cold ×0.11  bulk ×0.10  perop ×0.13  maint ×0.25   ✗ 0  ! 0  ⏱ 0
```

## Run 2

```
tsvzbench: probing py
tsvzbench: probing rs
tsvzbench 1.0  2026-10-14T17:56:18-0700  suite standard  seed 1
host ksxx  cpu AMD Ryzen 9 9950X3D 16-Core Processor ×32  Linux 7.0.14-23-pve  python 3.13.5  fs f2fs  warm page cache
  py           python3 /mnt/klein/work/cas/TSVZ/TSVZ.py  [TSVZ.py 4.8 @ 2026-10-14 by pan@zopyr.us]  handler: yes  startup 37.4 ms
  rs           /mnt/gen5/tsvzbench-rs/tsvz-release-candidate  [tsvz-rs 0.1.0]  handler: yes  startup 597 µs

[cold]
workload             | py wall  | ±  | net      | cpu      | rss       | ok | rs wall  | ±  | net      | cpu      | rss       | ok | ×
---------------------+----------+----+----------+----------+-----------+----+----------+----+----------+----------+-----------+----+------
startup              | 37.4 ms  | 1% |          | 36.6 ms  | 35.0 MiB  | ✓  | 597 µs   | 4% |          | 367 µs   | ≤8.8 MiB  | ✓  | ×0.02
read 10k             | 56.0 ms  | 0% | 18.6 ms  | 54.9 ms  | 41.3 MiB  | ✓  | 6.2 ms   | 3% | 5.6 ms   | 5.8 ms   | ≤8.8 MiB  | ✓  | ×0.11
len 10k              | 51.2 ms  | 1% | 13.8 ms  | 50.5 ms  | 38.6 MiB  | ✓  | 2.6 ms   | 2% | 2.0 ms   | 2.3 ms   | ≤8.8 MiB  | ✓  | ×0.05
get 10k              | 50.4 ms  | 0% | 13.0 ms  | 49.7 ms  | 38.8 MiB  | ✓  | 2.0 ms   | 3% | 1.4 ms   | 1.6 ms   | ≤8.8 MiB  | ✓  | ×0.04
read 100k            | 232.5 ms | 0% | 195.1 ms | 231.4 ms | 118.6 MiB | ✓  | 50.9 ms  | 1% | 50.3 ms  | 50.5 ms  | 37.4 MiB  | ✓  | ×0.22
len 100k             | 176.0 ms | 0% | 138.6 ms | 174.8 ms | 95.6 MiB  | ✓  | 14.8 ms  | 2% | 14.2 ms  | 14.5 ms  | 19.8 MiB  | ✓  | ×0.08
get 100k             | 166.2 ms | 1% | 128.8 ms | 165.3 ms | 89.4 MiB  | ✓  | 6.8 ms   | 1% | 6.2 ms   | 6.5 ms   | ≤8.8 MiB  | ✓  | ×0.04
read 1M              | 2.24 s   | 0% | 2.20 s   | 2.24 s   | 887.0 MiB | ✓  | 696.8 ms | 0% | 696.2 ms | 696.1 ms | 358.9 MiB | ✓  | ×0.31
len 1M               | 1.80 s   | 0% | 1.76 s   | 1.80 s   | 606.3 MiB | ✓  | 233.6 ms | 3% | 233.0 ms | 233.0 ms | 197.4 MiB | ✓  | ×0.13
get 1M               | 1.56 s   | 0% | 1.52 s   | 1.56 s   | 555.6 MiB | ✓  | 59.4 ms  | 0% | 58.8 ms  | 58.9 ms  | ≤8.8 MiB  | ✓  | ×0.04
read churn 100k      | 603.0 ms | 1% | 565.6 ms | 601.6 ms | 157.8 MiB | ✓  | 138.0 ms | 2% | 137.5 ms | 137.5 ms | 60.2 MiB  | ✓  | ×0.23
read escaped 100k    | 303.1 ms | 0% | 265.7 ms | 302.0 ms | 155.8 MiB | ✓  | 63.7 ms  | 0% | 63.1 ms  | 63.3 ms  | 37.8 MiB  | ✓  | ×0.21
read markers 100k    | 248.9 ms | 0% | 211.6 ms | 247.7 ms | 106.8 MiB | ✓  | 44.5 ms  | 2% | 43.9 ms  | 44.0 ms  | 29.7 MiB  | ✓  | ×0.18
read checksum 100k   | 262.3 ms | 0% | 224.9 ms | 261.1 ms | 119.1 MiB | ✓  | 51.4 ms  | 0% | 50.8 ms  | 51.0 ms  | 37.1 MiB  | ✓  | ×0.20
read gz 100k         | 248.4 ms | 0% | 211.0 ms | 247.3 ms | 119.4 MiB | ✓  | 68.1 ms  | 1% | 67.5 ms  | 67.6 ms  | 56.8 MiB  | ✓  | ×0.27
read multipart 100k  | 230.3 ms | 0% | 193.0 ms | 229.3 ms | 118.7 MiB | ✓  | 53.2 ms  | 0% | 52.6 ms  | 52.9 ms  | 38.2 MiB  | ✓  | ×0.23
verify checksum 100k | 171.3 ms | 1% | 133.9 ms | 170.2 ms | 38.5 MiB  | ✓  | 19.8 ms  | 0% | 19.2 ms  | 19.4 ms  | 19.5 MiB  | ✓  | ×0.12

[bulk]
workload       | py wall  | ±  | net      | cpu      | rss       | disk      | ok | rs wall  | ±   | net      | cpu      | rss       | disk      | ok | ×
---------------+----------+----+----------+----------+-----------+-----------+----+----------+-----+----------+----------+-----------+-----------+----+------
set - new 10k  | 52.9 ms  | 0% | 15.5 ms  | 52.0 ms  | 40.4 MiB  | 737.6 KiB | ✓  | 6.1 ms   | 4%  | 5.5 ms   | 5.8 ms   | ≤8.8 MiB  | 737.6 KiB | ✓  | ×0.11
set - new 100k | 184.1 ms | 0% | 146.7 ms | 183.0 ms | 101.0 MiB | 7.2 MiB   | ✓  | 52.4 ms  | 1%  | 51.8 ms  | 51.9 ms  | 60.2 MiB  | 7.2 MiB   | ✓  | ×0.28
set - new 1M   | 2.13 s   | 0% | 2.09 s   | 2.13 s   | 720.1 MiB | 72.0 MiB  | ✓  | 510.6 ms | 1%  | 510.0 ms | 510.0 ms | 581.1 MiB | 72.0 MiB  | ✓  | ×0.24
set - add 100k | 40.7 ms  | 1% | 3.3 ms   | 39.9 ms  | 35.1 MiB  | 7.3 MiB   | ✓  | 1.6 ms   | 6%  | 1.0 ms   | 1.3 ms   | ≤8.8 MiB  | 7.3 MiB   | ✓  | ×0.04
delete - 100k  | 45.3 ms  | 2% | 7.9 ms   | 44.4 ms  | 36.4 MiB  | 7.3 MiB   | ✓  | 1.6 ms   | 11% | 1.1 ms   | 1.3 ms   | ≤8.8 MiB  | 7.3 MiB   | ✓  | ×0.04

[perop]
workload            | py p50   | p95      | p99      | ops/s   | rss      | ok | rs p50 | p95    | p99    | ops/s    | rss      | ok | ×     | ceiling ops/s
--------------------+----------+----------+----------+---------+----------+----+--------+--------+--------+----------+----------+----+-------+--------------
cli set             | 39.0 ms  | 40.4 ms  | 41.6 ms  | 26/s    | 34.8 MiB | ✓  | 619 µs | 1.0 ms | 1.2 ms | 1.5k/s   | ≤8.8 MiB | ✓  | ×0.02 |
cli get             | 168.0 ms | 171.5 ms | 174.4 ms | 6/s     | 89.5 MiB | ✓  | 6.7 ms | 7.2 ms | 7.7 ms | 148/s    | ≤8.8 MiB | ✓  | ×0.04 |
cli via handler set | 41.5 ms  | 43.1 ms  | 43.5 ms  | 24/s    | 34.9 MiB | ✓  | 667 µs | 1.1 ms | 1.3 ms | 1.4k/s   | ≤8.8 MiB | ✓  | ×0.02 |
cli via handler get | 41.5 ms  | 43.0 ms  | 43.5 ms  | 24/s    | 34.9 MiB | ✓  | 635 µs | 907 µs | 1.0 ms | 1.5k/s   | ≤8.8 MiB | ✓  | ×0.02 |
socket get          | 19 µs    | 21 µs    | 27 µs    | 51.4k/s |          | ✓  | 7 µs   | 8 µs   | 12 µs  | 139.4k/s |          | ✓  | ×0.36 | 278.6k/s
socket set          | 10 µs    | 19 µs    | 25 µs    | 94.5k/s |          | ✓  | 9 µs   | 14 µs  | 30 µs  | 95.4k/s  |          | ✓  | ×0.91 | 248.1k/s
socket --sync set   | 713 µs   | 780 µs   | 947 µs   | 1.4k/s  |          | ✓  | 712 µs | 774 µs | 899 µs | 1.4k/s   |          | ✓  | ×1.00 | 248.5k/s
socket mix ×1       | 49 µs    | 90 µs    | 105 µs   | 25.9k/s |          | ✓  | 15 µs  | 22 µs  | 30 µs  | 62.4k/s  |          | ✓  | ×0.31 | 256.4k/s
socket mix ×4       | 124 µs   | 242 µs   | 468 µs   | 30.1k/s |          | ✓  | 37 µs  | 68 µs  | 89 µs  | 105.9k/s |          | ✓  | ×0.30 | 922.7k/s
socket mix ×16      | 565 µs   | 1.5 ms   | 2.2 ms   | 21.1k/s |          | ✓  | 156 µs | 333 µs | 518 µs | 102.5k/s |          | ✓  | ×0.28 | 1.77M/s

[maint]
workload             | py wall  | ±  | net      | cpu      | rss       | disk             | ok      | rs wall  | ±  | net      | cpu      | rss       | disk             | ok | ×
---------------------+----------+----+----------+----------+-----------+------------------+---------+----------+----+----------+----------+-----------+------------------+----+------
scrub churn 100k     | 718.0 ms | 1% | 680.6 ms | 713.7 ms | 170.8 MiB | 24.0 MiB→7.2 MiB | ✓       | 236.7 ms | 0% | 236.1 ms | 232.1 ms | 106.5 MiB | 24.0 MiB→7.2 MiB | ✓  | ×0.33
scrub markers 100k   | 268.6 ms | 0% | 231.2 ms | 265.3 ms | 107.0 MiB | 4.4 MiB→7.4 MiB  | ✓       | 69.3 ms  | 1% | 68.7 ms  | 66.3 ms  | 68.7 MiB  | 4.4 MiB→7.4 MiB  | ✓  | ×0.26
clear 100k           | 174.2 ms | 1% | 136.8 ms | 170.1 ms | 93.1 MiB  | 7.2 MiB→20 B     | ✓       | 41.2 ms  | 4% | 40.6 ms  | 39.2 ms  | 43.4 MiB  | 7.2 MiB→20 B     | ✓  | ×0.24
scrub multipart 100k | 166.6 ms | 0% | 129.2 ms | 165.3 ms | 87.3 MiB  | 7.2 MiB→7.2 MiB  | refused | 248.6 ms | 0% | 248.0 ms | 243.4 ms | 157.2 MiB | 7.2 MiB→13.5 MiB | ✓  | —

[footprint]
  py           memory/row (read) 896 B; memory/row (len) 601 B; disk/record 75 B; handler load 181.5 ms, rss after load 92.3 MiB, peak rss 96.5 MiB, cpu 6.04 s
  rs           memory/row (read) 372 B; memory/row (len) 203 B; disk/record 75 B; handler load 35.9 ms, rss after load 36.8 MiB, peak rss 45.0 MiB, cpu 1.90 s

[summary] ratio = time against py, geometric mean per family (lower is faster)
  py           cold ×1.00  bulk ×1.00  perop ×1.00  maint ×1.00   ✗ 0  ! 0  ⏱ 0
  rs           cold ×0.11  bulk ×0.10  perop ×0.13  maint ×0.27   ✗ 0  ! 0  ⏱ 0
```
