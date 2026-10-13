# tsvz-rs against TSVZ.py: tsvzbench results

Date: 2026-10-13. A ratio (×) is tsvz-rs's time over TSVZ.py's: below 1 is faster. Both runs
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
- Python 3.13.5; TSVZ.py 4.7 @ 2026-10-13 by pan@zopyr.us; tsvz-rs 0.1.0 (branch `handler`, commit c231a3d)

## Summary

tsvz-rs is faster than TSVZ.py on every cold, bulk, per-op and maint workload, in both runs, with
one tie: `socket --sync set`, where each request waits for its own `fdatasync`, takes the
device's flush latency in both tools (722-728 µs at p50; a bare append plus `fdatasync` in
Python on the same file system measures 720 µs at p50, 788 µs at p95).

- **cold** (one-shot reads (read, len, get, verify) of 10k-1M-row stores, plain, escaped, with markers, checksums, gzip and multi-part): median ratio ×0.13 in run 1 and ×0.18 in run 2; every workload between ×0.02 and ×0.36.
- **bulk** (set - and delete - batches of 10k-1M records): median ratio ×0.11 in both runs; every workload between ×0.03 and ×0.29.
- **perop** (single set and get commands, the same through each tool's handler, and requests straight to the handler socket, alone and from 4 and 16 clients): median ratio ×0.21 in both runs; every workload between ×0.02 and ×0.41, except `socket --sync set` at ×1.00 (see above). A command through the handler takes 0.7-0.8 ms against 42 ms; the handler answers a `get` in 7 µs against 19 µs, and 16 clients mixing requests get 131-136k requests/s against 26-29k.
- **maint** (scrub and clear): median ratio ×0.23 in run 1 and ×0.25 in run 2; every workload between ×0.22 and ×0.38.

`scrub multipart 100k` has no ratio: both tools refuse to scrub a multi-part store (sub-project 3).

The handler loads a 100k-row store in 36-41 ms against 175-179 ms, holding 37 MiB against 92 MiB.

The last minute of run 2 (its maint family, 01:27-01:28) overlapped a whole-file-system `find`
run by another process; run 1 was undisturbed, and the two runs agree on every conclusion.

## Run 1

```
tsvzbench 1.0  2026-10-13T01:20:17-0700  suite standard  seed 1
host ksxx  cpu AMD Ryzen 9 9950X3D 16-Core Processor ×32  Linux 7.0.14-23-pve  python 3.13.5  fs f2fs  warm page cache
  py           python3 ../TSVZ/TSVZ.py  [TSVZ.py 4.7 @ 2026-10-13 by pan@zopyr.us]  handler: yes  startup 38.4 ms
  rs           target/release/tsvz  [tsvz-rs 0.1.0]  handler: yes  startup 654 µs

[cold]
workload             | py wall  | ±  | net      | cpu      | rss       | ok | rs wall  | ±  | net      | cpu      | rss       | ok | ×
---------------------+----------+----+----------+----------+-----------+----+----------+----+----------+----------+-----------+----+------
startup              | 38.4 ms  | 3% |          | 37.6 ms  | 34.6 MiB  | ✓  | 654 µs   | 5% |          | 366 µs   | ≤8.8 MiB  | ✓  | ×0.02
read 10k             | 57.4 ms  | 1% | 19.0 ms  | 56.5 ms  | 41.3 MiB  | ✓  | 6.4 ms   | 1% | 5.7 ms   | 5.9 ms   | ≤8.8 MiB  | ✓  | ×0.11
len 10k              | 50.9 ms  | 1% | 12.5 ms  | 50.0 ms  | 39.0 MiB  | ✓  | 2.5 ms   | 6% | 1.8 ms   | 2.2 ms   | ≤8.8 MiB  | ✓  | ×0.05
get 10k              | 51.3 ms  | 2% | 12.9 ms  | 50.7 ms  | 38.6 MiB  | ✓  | 1.8 ms   | 7% | 1.1 ms   | 1.5 ms   | ≤8.8 MiB  | ✓  | ×0.03
read 100k            | 240.8 ms | 1% | 202.4 ms | 239.6 ms | 119.0 MiB | ✓  | 53.2 ms  | 1% | 52.6 ms  | 52.9 ms  | 37.1 MiB  | ✓  | ×0.22
len 100k             | 181.4 ms | 4% | 143.0 ms | 180.5 ms | 95.3 MiB  | ✓  | 15.2 ms  | 5% | 14.6 ms  | 14.8 ms  | 19.7 MiB  | ✓  | ×0.08
get 100k             | 173.2 ms | 1% | 134.8 ms | 172.0 ms | 89.3 MiB  | ✓  | 6.8 ms   | 1% | 6.1 ms   | 6.4 ms   | ≤8.8 MiB  | ✓  | ×0.04
read 1M              | 2.24 s   | 2% | 2.20 s   | 2.24 s   | 887.0 MiB | ✓  | 760.9 ms | 5% | 760.3 ms | 760.5 ms | 358.8 MiB | ✓  | ×0.34
len 1M               | 1.80 s   | 1% | 1.76 s   | 1.80 s   | 605.8 MiB | ✓  | 235.7 ms | 2% | 235.0 ms | 235.2 ms | 197.5 MiB | ✓  | ×0.13
get 1M               | 1.60 s   | 1% | 1.56 s   | 1.60 s   | 555.5 MiB | ✓  | 57.7 ms  | 1% | 57.1 ms  | 57.2 ms  | ≤8.8 MiB  | ✓  | ×0.04
read churn 100k      | 660.1 ms | 6% | 621.7 ms | 658.9 ms | 157.8 MiB | ✓  | 145.9 ms | 3% | 145.2 ms | 145.4 ms | 60.1 MiB  | ✓  | ×0.22
read escaped 100k    | 302.0 ms | 1% | 263.6 ms | 300.9 ms | 155.4 MiB | ✓  | 62.9 ms  | 0% | 62.2 ms  | 62.5 ms  | 37.7 MiB  | ✓  | ×0.21
read markers 100k    | 247.2 ms | 0% | 208.8 ms | 246.2 ms | 106.9 MiB | ✓  | 43.3 ms  | 1% | 42.7 ms  | 42.8 ms  | 29.6 MiB  | ✓  | ×0.18
read checksum 100k   | 263.6 ms | 0% | 225.2 ms | 262.6 ms | 119.0 MiB | ✓  | 51.3 ms  | 1% | 50.6 ms  | 50.8 ms  | 37.4 MiB  | ✓  | ×0.19
read gz 100k         | 251.7 ms | 1% | 213.3 ms | 250.7 ms | 119.2 MiB | ✓  | 67.8 ms  | 3% | 67.2 ms  | 67.3 ms  | 56.8 MiB  | ✓  | ×0.27
read multipart 100k  | 228.4 ms | 1% | 190.0 ms | 227.3 ms | 119.1 MiB | ✓  | 51.8 ms  | 1% | 51.2 ms  | 51.4 ms  | 38.1 MiB  | ✓  | ×0.23
verify checksum 100k | 171.1 ms | 0% | 132.7 ms | 170.4 ms | 38.5 MiB  | ✓  | 19.2 ms  | 1% | 18.6 ms  | 18.9 ms  | 19.5 MiB  | ✓  | ×0.11

[bulk]
workload       | py wall  | ±  | net      | cpu      | rss       | disk      | ok | rs wall  | ±  | net      | cpu      | rss       | disk      | ok | ×
---------------+----------+----+----------+----------+-----------+-----------+----+----------+----+----------+----------+-----------+-----------+----+------
set - new 10k  | 52.5 ms  | 1% | 14.1 ms  | 51.7 ms  | 40.0 MiB  | 737.6 KiB | ✓  | 5.8 ms   | 4% | 5.1 ms   | 5.3 ms   | ≤8.8 MiB  | 737.6 KiB | ✓  | ×0.11
set - new 100k | 183.0 ms | 1% | 144.6 ms | 182.0 ms | 101.5 MiB | 7.2 MiB   | ✓  | 51.3 ms  | 1% | 50.6 ms  | 50.7 ms  | 60.1 MiB  | 7.2 MiB   | ✓  | ×0.28
set - new 1M   | 2.20 s   | 0% | 2.17 s   | 2.20 s   | 719.4 MiB | 72.0 MiB  | ✓  | 529.4 ms | 1% | 528.7 ms | 528.6 ms | 581.2 MiB | 72.0 MiB  | ✓  | ×0.24
set - add 100k | 40.6 ms  | 1% | 2.2 ms   | 39.8 ms  | 35.3 MiB  | 7.3 MiB   | ✓  | 1.3 ms   | 5% | 641 µs   | 980 µs   | ≤8.8 MiB  | 7.3 MiB   | ✓  | ×0.03
delete - 100k  | 45.4 ms  | 1% | 7.0 ms   | 44.6 ms  | 36.3 MiB  | 7.3 MiB   | ✓  | 1.7 ms   | 3% | 996 µs   | 1.3 ms   | ≤8.8 MiB  | 7.3 MiB   | ✓  | ×0.04

[perop]
workload            | py p50   | p95      | p99      | ops/s   | rss      | ok | rs p50 | p95    | p99    | ops/s    | rss      | ok | ×     | ceiling ops/s
--------------------+----------+----------+----------+---------+----------+----+--------+--------+--------+----------+----------+----+-------+--------------
cli set             | 39.5 ms  | 43.0 ms  | 96.4 ms  | 25/s    | 34.9 MiB | ✓  | 654 µs | 997 µs | 1.3 ms | 1.5k/s   | ≤8.8 MiB | ✓  | ×0.02 |
cli get             | 172.2 ms | 184.2 ms | 224.2 ms | 6/s     | 89.4 MiB | ✓  | 6.6 ms | 7.1 ms | 7.3 ms | 150/s    | ≤8.8 MiB | ✓  | ×0.04 |
cli via handler set | 43.0 ms  | 47.4 ms  | 51.3 ms  | 23/s    | 35.0 MiB | ✓  | 759 µs | 1.1 ms | 1.2 ms | 1.3k/s   | ≤8.8 MiB | ✓  | ×0.02 |
cli via handler get | 42.7 ms  | 45.0 ms  | 46.4 ms  | 23/s    | 35.0 MiB | ✓  | 706 µs | 981 µs | 1.1 ms | 1.3k/s   | ≤8.8 MiB | ✓  | ×0.02 |
socket get          | 19 µs    | 22 µs    | 29 µs    | 50.0k/s |          | ✓  | 7 µs   | 9 µs   | 12 µs  | 130.6k/s |          | ✓  | ×0.37 | 246.3k/s
socket set          | 10 µs    | 14 µs    | 19 µs    | 95.2k/s |          | ✓  | 4 µs   | 6 µs   | 8 µs   | 227.3k/s |          | ✓  | ×0.41 | 247.4k/s
socket --sync set   | 728 µs   | 797 µs   | 935 µs   | 1.4k/s  |          | ✓  | 728 µs | 796 µs | 935 µs | 1.4k/s   |          | ✓  | ×1.00 | 253.9k/s
socket mix ×1       | 50 µs    | 94 µs    | 130 µs   | 22.6k/s |          | ✓  | 11 µs  | 18 µs  | 22 µs  | 81.6k/s  |          | ✓  | ×0.22 | 250.1k/s
socket mix ×4       | 129 µs   | 255 µs   | 455 µs   | 29.0k/s |          | ✓  | 27 µs  | 46 µs  | 54 µs  | 143.0k/s |          | ✓  | ×0.21 | 873.5k/s
socket mix ×16      | 446 µs   | 1.3 ms   | 1.9 ms   | 29.2k/s |          | ✓  | 129 µs | 182 µs | 267 µs | 130.6k/s |          | ✓  | ×0.29 | 1.62M/s

[maint]
workload             | py wall  | ±  | net      | cpu      | rss       | disk             | ok      | rs wall  | ±   | net      | cpu      | rss       | disk             | ok      | ×
---------------------+----------+----+----------+----------+-----------+------------------+---------+----------+-----+----------+----------+-----------+------------------+---------+------
scrub churn 100k     | 694.5 ms | 3% | 656.1 ms | 690.2 ms | 170.9 MiB | 24.0 MiB→7.2 MiB | ✓       | 190.8 ms | 9%  | 190.2 ms | 188.7 ms | 106.8 MiB | 24.0 MiB→7.2 MiB | ✓       | ×0.27
scrub markers 100k   | 272.1 ms | 0% | 233.8 ms | 268.4 ms | 107.0 MiB | 4.4 MiB→7.4 MiB  | ✓       | 63.3 ms  | 2%  | 62.7 ms  | 61.2 ms  | 68.4 MiB  | 4.4 MiB→7.4 MiB  | ✓       | ×0.23
clear 100k           | 177.0 ms | 2% | 138.6 ms | 173.0 ms | 93.0 MiB  | 7.2 MiB→20 B     | ✓       | 38.1 ms  | 1%  | 37.5 ms  | 36.0 ms  | 43.2 MiB  | 7.2 MiB→20 B     | ✓       | ×0.22
scrub multipart 100k | 169.0 ms | 1% | 130.6 ms | 168.1 ms | 87.2 MiB  | 7.2 MiB→7.2 MiB  | refused | 1.0 ms   | 18% | 376 µs   | 712 µs   | ≤8.8 MiB  | 7.2 MiB→7.2 MiB  | refused | —

[footprint]
  py           memory/row (read) 896 B; memory/row (len) 600 B; disk/record 75 B; handler load 178.5 ms, rss after load 92.3 MiB, peak rss 96.2 MiB, cpu 4.95 s
  rs           memory/row (read) 372 B; memory/row (len) 203 B; disk/record 75 B; handler load 40.8 ms, rss after load 36.8 MiB, peak rss 45.5 MiB, cpu 1.45 s

[summary] ratio = time against py, geometric mean per family (lower is faster)
  py           cold ×1.00  bulk ×1.00  perop ×1.00  maint ×1.00   ✗ 0  ! 0  ⏱ 0
  rs           cold ×0.11  bulk ×0.10  perop ×0.11  maint ×0.24   ✗ 0  ! 0  ⏱ 0
```

## Run 2

```
tsvzbench 1.0  2026-10-13T01:24:00-0700  suite standard  seed 1
host ksxx  cpu AMD Ryzen 9 9950X3D 16-Core Processor ×32  Linux 7.0.14-23-pve  python 3.13.5  fs f2fs  warm page cache
  py           python3 ../TSVZ/TSVZ.py  [TSVZ.py 4.7 @ 2026-10-13 by pan@zopyr.us]  handler: yes  startup 41.8 ms
  rs           target/release/tsvz  [tsvz-rs 0.1.0]  handler: yes  startup 896 µs

[cold]
workload             | py wall  | ±  | net      | cpu      | rss       | ok | rs wall  | ±   | net      | cpu      | rss       | ok | ×
---------------------+----------+----+----------+----------+-----------+----+----------+-----+----------+----------+-----------+----+------
startup              | 41.8 ms  | 1% |          | 41.1 ms  | 34.8 MiB  | ✓  | 896 µs   | 5%  |          | 574 µs   | ≤8.9 MiB  | ✓  | ×0.02
read 10k             | 59.6 ms  | 2% | 17.8 ms  | 59.0 ms  | 40.9 MiB  | ✓  | 6.5 ms   | 4%  | 5.6 ms   | 6.1 ms   | ≤8.8 MiB  | ✓  | ×0.11
len 10k              | 53.0 ms  | 2% | 11.2 ms  | 52.5 ms  | 39.2 MiB  | ✓  | 2.5 ms   | 10% | 1.6 ms   | 2.2 ms   | ≤8.9 MiB  | ✓  | ×0.05
get 10k              | 51.9 ms  | 2% | 10.1 ms  | 51.2 ms  | 38.5 MiB  | ✓  | 1.6 ms   | 3%  | 712 µs   | 1.3 ms   | ≤8.8 MiB  | ✓  | ×0.03
read 100k            | 236.4 ms | 0% | 194.6 ms | 235.6 ms | 119.1 MiB | ✓  | 55.3 ms  | 2%  | 54.4 ms  | 54.8 ms  | 37.2 MiB  | ✓  | ×0.23
len 100k             | 189.1 ms | 1% | 147.2 ms | 188.1 ms | 95.6 MiB  | ✓  | 15.3 ms  | 3%  | 14.4 ms  | 14.9 ms  | 19.6 MiB  | ✓  | ×0.08
get 100k             | 172.4 ms | 1% | 130.6 ms | 171.7 ms | 89.5 MiB  | ✓  | 7.0 ms   | 3%  | 6.1 ms   | 6.7 ms   | ≤8.8 MiB  | ✓  | ×0.04
read 1M              | 2.32 s   | 1% | 2.28 s   | 2.32 s   | 886.7 MiB | ✓  | 842.0 ms | 5%  | 841.1 ms | 841.4 ms | 358.8 MiB | ✓  | ×0.36
len 1M               | 2.00 s   | 4% | 1.96 s   | 2.00 s   | 606.2 MiB | ✓  | 388.5 ms | 2%  | 387.6 ms | 387.9 ms | 197.5 MiB | ✓  | ×0.19
get 1M               | 1.60 s   | 1% | 1.55 s   | 1.60 s   | 555.3 MiB | ✓  | 59.6 ms  | 1%  | 58.8 ms  | 59.2 ms  | ≤8.8 MiB  | ✓  | ×0.04
read churn 100k      | 656.4 ms | 1% | 614.6 ms | 655.2 ms | 157.3 MiB | ✓  | 158.4 ms | 9%  | 157.6 ms | 157.8 ms | 59.9 MiB  | ✓  | ×0.24
read escaped 100k    | 313.9 ms | 3% | 272.1 ms | 312.8 ms | 155.1 MiB | ✓  | 73.6 ms  | 4%  | 72.7 ms  | 73.1 ms  | 37.9 MiB  | ✓  | ×0.23
read markers 100k    | 249.5 ms | 1% | 207.6 ms | 248.3 ms | 106.7 MiB | ✓  | 44.3 ms  | 1%  | 43.4 ms  | 43.6 ms  | 29.7 MiB  | ✓  | ×0.18
read checksum 100k   | 273.1 ms | 1% | 231.3 ms | 272.2 ms | 119.2 MiB | ✓  | 52.6 ms  | 2%  | 51.7 ms  | 52.3 ms  | 37.2 MiB  | ✓  | ×0.19
read gz 100k         | 254.7 ms | 1% | 212.9 ms | 253.6 ms | 119.5 MiB | ✓  | 67.2 ms  | 1%  | 66.3 ms  | 66.7 ms  | 56.7 MiB  | ✓  | ×0.26
read multipart 100k  | 232.2 ms | 0% | 190.3 ms | 231.0 ms | 118.4 MiB | ✓  | 53.9 ms  | 0%  | 53.0 ms  | 53.4 ms  | 38.2 MiB  | ✓  | ×0.23
verify checksum 100k | 174.1 ms | 1% | 132.3 ms | 173.3 ms | 38.8 MiB  | ✓  | 20.6 ms  | 2%  | 19.7 ms  | 20.0 ms  | 19.6 MiB  | ✓  | ×0.12

[bulk]
workload       | py wall  | ±  | net      | cpu      | rss       | disk      | ok | rs wall  | ±  | net      | cpu      | rss       | disk      | ok | ×
---------------+----------+----+----------+----------+-----------+-----------+----+----------+----+----------+----------+-----------+-----------+----+------
set - new 10k  | 57.9 ms  | 7% | 16.1 ms  | 57.0 ms  | 40.2 MiB  | 737.6 KiB | ✓  | 6.4 ms   | 4% | 5.5 ms   | 6.0 ms   | ≤8.8 MiB  | 737.6 KiB | ✓  | ×0.11
set - new 100k | 188.6 ms | 1% | 146.8 ms | 187.8 ms | 100.9 MiB | 7.2 MiB   | ✓  | 54.5 ms  | 2% | 53.6 ms  | 54.0 ms  | 60.0 MiB  | 7.2 MiB   | ✓  | ×0.29
set - new 1M   | 2.22 s   | 2% | 2.18 s   | 2.22 s   | 719.4 MiB | 72.0 MiB  | ✓  | 531.5 ms | 1% | 530.6 ms | 531.0 ms | 581.1 MiB | 72.0 MiB  | ✓  | ×0.24
set - add 100k | 40.8 ms  | 2% | ≈0       | 40.1 ms  | 35.1 MiB  | 7.3 MiB   | ✓  | 1.3 ms   | 0% | 368 µs   | 989 µs   | ≤8.8 MiB  | 7.3 MiB   | ✓  | ×0.03
delete - 100k  | 46.8 ms  | 2% | 4.9 ms   | 45.9 ms  | 36.5 MiB  | 7.3 MiB   | ✓  | 2.0 ms   | 2% | 1.1 ms   | 1.7 ms   | ≤8.9 MiB  | 7.3 MiB   | ✓  | ×0.04

[perop]
workload            | py p50   | p95      | p99      | ops/s   | rss      | ok | rs p50 | p95    | p99    | ops/s    | rss      | ok | ×     | ceiling ops/s
--------------------+----------+----------+----------+---------+----------+----+--------+--------+--------+----------+----------+----+-------+--------------
cli set             | 39.3 ms  | 43.0 ms  | 48.5 ms  | 25/s    | 34.8 MiB | ✓  | 684 µs | 1.1 ms | 1.2 ms | 1.4k/s   | ≤8.8 MiB | ✓  | ×0.02 |
cli get             | 173.3 ms | 185.4 ms | 188.8 ms | 6/s     | 89.4 MiB | ✓  | 6.6 ms | 7.4 ms | 7.5 ms | 149/s    | ≤8.8 MiB | ✓  | ×0.04 |
cli via handler set | 41.8 ms  | 46.4 ms  | 60.4 ms  | 24/s    | 34.9 MiB | ✓  | 765 µs | 1.1 ms | 1.3 ms | 1.2k/s   | ≤8.8 MiB | ✓  | ×0.02 |
cli via handler get | 42.0 ms  | 46.3 ms  | 63.6 ms  | 24/s    | 34.9 MiB | ✓  | 721 µs | 1.1 ms | 1.2 ms | 1.3k/s   | ≤8.8 MiB | ✓  | ×0.02 |
socket get          | 19 µs    | 21 µs    | 27 µs    | 50.8k/s |          | ✓  | 7 µs   | 9 µs   | 12 µs  | 134.6k/s |          | ✓  | ×0.36 | 275.4k/s
socket set          | 10 µs    | 19 µs    | 25 µs    | 93.8k/s |          | ✓  | 4 µs   | 6 µs   | 7 µs   | 238.1k/s |          | ✓  | ×0.40 | 227.6k/s
socket --sync set   | 722 µs   | 796 µs   | 973 µs   | 1.4k/s  |          | ✓  | 723 µs | 861 µs | 1.5 ms | 1.3k/s   |          | ✓  | ×1.00 | 245.8k/s
socket mix ×1       | 49 µs    | 88 µs    | 109 µs   | 25.8k/s |          | ✓  | 11 µs  | 15 µs  | 18 µs  | 97.8k/s  |          | ✓  | ×0.22 | 271.9k/s
socket mix ×4       | 129 µs   | 267 µs   | 483 µs   | 29.1k/s |          | ✓  | 26 µs  | 42 µs  | 50 µs  | 148.4k/s |          | ✓  | ×0.20 | 927.5k/s
socket mix ×16      | 482 µs   | 1.3 ms   | 1.9 ms   | 25.8k/s |          | ✓  | 124 µs | 174 µs | 256 µs | 135.5k/s |          | ✓  | ×0.26 | 2.28M/s

[maint]
workload             | py wall  | ±  | net      | cpu      | rss       | disk             | ok      | rs wall  | ±  | net      | cpu      | rss       | disk             | ok      | ×
---------------------+----------+----+----------+----------+-----------+------------------+---------+----------+----+----------+----------+-----------+------------------+---------+------
scrub churn 100k     | 677.2 ms | 3% | 635.4 ms | 675.2 ms | 169.8 MiB | 24.0 MiB→7.2 MiB | ✓       | 255.3 ms | 2% | 254.4 ms | 253.8 ms | 106.4 MiB | 24.0 MiB→7.2 MiB | ✓       | ×0.38
scrub markers 100k   | 279.8 ms | 3% | 238.0 ms | 272.7 ms | 106.7 MiB | 4.4 MiB→7.4 MiB  | ✓       | 70.6 ms  | 2% | 69.7 ms  | 68.1 ms  | 68.5 MiB  | 4.4 MiB→7.4 MiB  | ✓       | ×0.25
clear 100k           | 171.5 ms | 1% | 129.7 ms | 168.4 ms | 93.0 MiB  | 7.2 MiB→20 B     | ✓       | 38.8 ms  | 2% | 37.9 ms  | 36.7 ms  | 43.1 MiB  | 7.2 MiB→20 B     | ✓       | ×0.23
scrub multipart 100k | 169.0 ms | 2% | 127.1 ms | 168.1 ms | 87.1 MiB  | 7.2 MiB→7.2 MiB  | refused | 747 µs   | 6% | ≈0       | 468 µs   | ≤8.8 MiB  | 7.2 MiB→7.2 MiB  | refused | —

[footprint]
  py           memory/row (read) 896 B; memory/row (len) 601 B; disk/record 75 B; handler load 175.2 ms, rss after load 92.3 MiB, peak rss 96.0 MiB, cpu 5.31 s
  rs           memory/row (read) 372 B; memory/row (len) 203 B; disk/record 75 B; handler load 35.8 ms, rss after load 36.8 MiB, peak rss 45.9 MiB, cpu 1.38 s

[summary] ratio = time against py, geometric mean per family (lower is faster)
  py           cold ×1.00  bulk ×1.00  perop ×1.00  maint ×1.00   ✗ 0  ! 0  ⏱ 0
  rs           cold ×0.11  bulk ×0.10  perop ×0.11  maint ×0.28   ✗ 0  ! 0  ⏱ 0
```
