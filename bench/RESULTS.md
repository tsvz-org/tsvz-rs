# tsvz-rs against TSVZ.py: tsvzbench results

Date: 2026-10-12. A ratio (×) is tsvz-rs's time over TSVZ.py's: below 1 is faster. Both runs
used the same fixtures (seed 1) on a warm page cache, with the work directory on f2fs.

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
- Python 3.13.5; TSVZ.py 4.5 @ 2026-10-12 by pan@zopyr.us; tsvz-rs 0.1.0 (commit 4d63986)

## Summary

tsvz-rs is faster than TSVZ.py on every cold, bulk, per-op CLI and maint workload, in both runs.
Most of the per-op gap is start-up: 0.9 ms against 38 ms.

- **cold** (one-shot reads (read, len, get, verify) of 10k-1M-row stores, plain, escaped, with markers, checksums, gzip and multi-part): median ratio ×0.18 in run 1 and ×0.13 in run 2; every workload between ×0.02 and ×0.35.
- **bulk** (set - and delete - batches of 10k-1M records): median ratio ×0.11 in run 1 and ×0.12 in run 2; every workload between ×0.03 and ×0.28.
- **perop** (single set and get commands (the handler rows are n/a: tsvz-rs has no handler yet)): median ratio ×0.03 in run 1 and ×0.03 in run 2; every workload between ×0.02 and ×0.04.
- **maint** (scrub and clear): median ratio ×0.25 in run 1 and ×0.24 in run 2; every workload between ×0.23 and ×0.33.

`scrub multipart 100k` has no ratio: both tools refuse to scrub a multi-part store (sub-project 3).

## Run 1

```
tsvzbench 1.0  2026-10-12T19:38:55-0700  suite standard  seed 1
host ksxx  cpu AMD Ryzen 9 9950X3D 16-Core Processor ×32  Linux 7.0.14-23-pve  python 3.13.5  fs f2fs  warm page cache
  py           python3 ../TSVZ/TSVZ.py  [TSVZ.py 4.5 @ 2026-10-12 by pan@zopyr.us]  handler: yes  startup 37.8 ms
  rs           target/release/tsvz  [tsvz-rs 0.1.0]  handler: no (serve exited with status 1)  startup 858 µs

[cold]
workload             | py wall  | ±  | net      | cpu      | rss       | ok | rs wall  | ±   | net      | cpu      | rss       | ok | ×
---------------------+----------+----+----------+----------+-----------+----+----------+-----+----------+----------+-----------+----+------
startup              | 37.8 ms  | 1% |          | 36.8 ms  | 34.1 MiB  | ✓  | 858 µs   | 7%  |          | 586 µs   | ≤8.8 MiB  | ✓  | ×0.02
read 10k             | 56.0 ms  | 1% | 18.2 ms  | 55.2 ms  | 40.1 MiB  | ✓  | 6.2 ms   | 4%  | 5.4 ms   | 5.9 ms   | ≤8.8 MiB  | ✓  | ×0.11
len 10k              | 50.4 ms  | 0% | 12.6 ms  | 49.6 ms  | 38.3 MiB  | ✓  | 2.4 ms   | 8%  | 1.6 ms   | 1.9 ms   | ≤8.9 MiB  | ✓  | ×0.05
get 10k              | 49.9 ms  | 2% | 12.1 ms  | 49.0 ms  | 38.0 MiB  | ✓  | 1.6 ms   | 10% | 786 µs   | 1.3 ms   | ≤8.8 MiB  | ✓  | ×0.03
read 100k            | 229.7 ms | 1% | 192.0 ms | 228.5 ms | 118.3 MiB | ✓  | 53.1 ms  | 1%  | 52.3 ms  | 52.5 ms  | 37.0 MiB  | ✓  | ×0.23
len 100k             | 183.3 ms | 1% | 145.6 ms | 182.2 ms | 95.1 MiB  | ✓  | 14.8 ms  | 1%  | 13.9 ms  | 14.3 ms  | 19.4 MiB  | ✓  | ×0.08
get 100k             | 167.0 ms | 1% | 129.2 ms | 166.0 ms | 88.7 MiB  | ✓  | 7.1 ms   | 3%  | 6.3 ms   | 6.6 ms   | ≤8.8 MiB  | ✓  | ×0.04
read 1M              | 2.25 s   | 0% | 2.21 s   | 2.25 s   | 886.3 MiB | ✓  | 785.5 ms | 1%  | 784.6 ms | 784.8 ms | 358.5 MiB | ✓  | ×0.35
len 1M               | 1.83 s   | 2% | 1.79 s   | 1.83 s   | 605.5 MiB | ✓  | 331.2 ms | 1%  | 330.4 ms | 330.5 ms | 197.3 MiB | ✓  | ×0.18
get 1M               | 1.61 s   | 1% | 1.57 s   | 1.61 s   | 555.4 MiB | ✓  | 58.6 ms  | 2%  | 57.8 ms  | 58.3 ms  | ≤8.8 MiB  | ✓  | ×0.04
read churn 100k      | 655.3 ms | 1% | 617.6 ms | 654.2 ms | 157.2 MiB | ✓  | 187.2 ms | 0%  | 186.3 ms | 186.4 ms | 60.0 MiB  | ✓  | ×0.29
read escaped 100k    | 304.7 ms | 0% | 267.0 ms | 303.8 ms | 155.1 MiB | ✓  | 65.4 ms  | 1%  | 64.6 ms  | 64.8 ms  | 37.6 MiB  | ✓  | ×0.21
read markers 100k    | 248.4 ms | 0% | 210.6 ms | 247.5 ms | 106.2 MiB | ✓  | 45.4 ms  | 1%  | 44.5 ms  | 44.9 ms  | 29.5 MiB  | ✓  | ×0.18
read checksum 100k   | 262.9 ms | 0% | 225.1 ms | 261.9 ms | 118.9 MiB | ✓  | 53.6 ms  | 0%  | 52.8 ms  | 53.1 ms  | 36.9 MiB  | ✓  | ×0.20
read gz 100k         | 249.4 ms | 0% | 211.6 ms | 248.1 ms | 118.9 MiB | ✓  | 69.0 ms  | 1%  | 68.1 ms  | 68.5 ms  | 56.6 MiB  | ✓  | ×0.28
read multipart 100k  | 227.3 ms | 0% | 189.5 ms | 226.2 ms | 118.4 MiB | ✓  | 54.0 ms  | 1%  | 53.1 ms  | 53.3 ms  | 38.0 MiB  | ✓  | ×0.24
verify checksum 100k | 167.5 ms | 0% | 129.7 ms | 166.3 ms | 38.1 MiB  | ✓  | 20.1 ms  | 1%  | 19.3 ms  | 19.8 ms  | 19.3 MiB  | ✓  | ×0.12

[bulk]
workload       | py wall  | ±  | net      | cpu      | rss       | disk      | ok | rs wall  | ±  | net      | cpu      | rss       | disk      | ok | ×
---------------+----------+----+----------+----------+-----------+-----------+----+----------+----+----------+----------+-----------+-----------+----+------
set - new 10k  | 53.3 ms  | 1% | 15.6 ms  | 52.4 ms  | 39.9 MiB  | 737.6 KiB | ✓  | 5.9 ms   | 3% | 5.0 ms   | 5.5 ms   | ≤8.8 MiB  | 737.6 KiB | ✓  | ×0.11
set - new 100k | 186.6 ms | 1% | 148.9 ms | 185.3 ms | 100.3 MiB | 7.2 MiB   | ✓  | 53.1 ms  | 1% | 52.3 ms  | 52.7 ms  | 60.1 MiB  | 7.2 MiB   | ✓  | ×0.28
set - new 1M   | 2.19 s   | 1% | 2.15 s   | 2.18 s   | 719.1 MiB | 72.0 MiB  | ✓  | 511.4 ms | 0% | 510.6 ms | 510.8 ms | 580.9 MiB | 72.0 MiB  | ✓  | ×0.23
set - add 100k | 40.0 ms  | 3% | 2.2 ms   | 39.2 ms  | 34.2 MiB  | 7.3 MiB   | ✓  | 1.3 ms   | 4% | 411 µs   | 985 µs   | ≤8.9 MiB  | 7.3 MiB   | ✓  | ×0.03
delete - 100k  | 43.9 ms  | 1% | 6.2 ms   | 43.1 ms  | 34.6 MiB  | 7.3 MiB   | ✓  | 1.8 ms   | 4% | 982 µs   | 1.4 ms   | ≤8.8 MiB  | 7.3 MiB   | ✓  | ×0.04

[perop]
workload            | py p50   | p95      | p99      | ops/s   | rss      | ok | rs p50 | p95    | p99    | ops/s  | rss      | ok  | ×     | ceiling ops/s
--------------------+----------+----------+----------+---------+----------+----+--------+--------+--------+--------+----------+-----+-------+--------------
cli set             | 38.4 ms  | 39.8 ms  | 40.1 ms  | 26/s    | 34.2 MiB | ✓  | 638 µs | 977 µs | 1.2 ms | 1.4k/s | ≤8.8 MiB | ✓   | ×0.02 |
cli get             | 168.6 ms | 171.2 ms | 174.7 ms | 6/s     | 88.8 MiB | ✓  | 6.5 ms | 7.0 ms | 7.3 ms | 151/s  | ≤8.8 MiB | ✓   | ×0.04 |
cli via handler set | 40.8 ms  | 42.2 ms  | 43.3 ms  | 25/s    | 34.4 MiB | ✓  | —      | —      | —      | —      |          | n/a | —     |
cli via handler get | 40.7 ms  | 42.0 ms  | 42.5 ms  | 25/s    | 34.3 MiB | ✓  | —      | —      | —      | —      |          | n/a | —     |
socket get          | 19 µs    | 21 µs    | 26 µs    | 50.4k/s |          | ✓  | —      | —      | —      | —      |          | n/a | —     | 272.1k/s
socket set          | 11 µs    | 21 µs    | 26 µs    | 81.6k/s |          | ✓  | —      | —      | —      | —      |          | n/a | —     | 232.1k/s
socket --sync set   | 705 µs   | 775 µs   | 948 µs   | 1.4k/s  |          | ✓  | —      | —      | —      | —      |          | n/a | —     | 245.6k/s
socket mix ×1       | 48 µs    | 61 µs    | 85 µs    | 27.2k/s |          | ✓  | —      | —      | —      | —      |          | n/a | —     | 234.6k/s
socket mix ×4       | 125 µs   | 243 µs   | 477 µs   | 29.4k/s |          | ✓  | —      | —      | —      | —      |          | n/a | —     | 901.9k/s
socket mix ×16      | 535 µs   | 1.4 ms   | 2.1 ms   | 24.1k/s |          | ✓  | —      | —      | —      | —      |          | n/a | —     | 1.88M/s

[maint]
workload             | py wall  | ±  | net      | cpu      | rss       | disk             | ok      | rs wall  | ±  | net      | cpu      | rss       | disk             | ok      | ×
---------------------+----------+----+----------+----------+-----------+------------------+---------+----------+----+----------+----------+-----------+------------------+---------+------
scrub churn 100k     | 716.5 ms | 0% | 678.7 ms | 711.8 ms | 157.0 MiB | 24.0 MiB→7.2 MiB | ✓       | 237.6 ms | 3% | 236.7 ms | 233.7 ms | 106.4 MiB | 24.0 MiB→7.2 MiB | ✓       | ×0.33
scrub markers 100k   | 269.2 ms | 1% | 231.4 ms | 265.2 ms | 106.4 MiB | 4.4 MiB→7.4 MiB  | ✓       | 67.7 ms  | 2% | 66.9 ms  | 64.3 ms  | 68.4 MiB  | 4.4 MiB→7.4 MiB  | ✓       | ×0.25
clear 100k           | 171.8 ms | 0% | 134.0 ms | 167.6 ms | 88.8 MiB  | 7.2 MiB→20 B     | ✓       | 40.2 ms  | 2% | 39.3 ms  | 38.0 ms  | 43.1 MiB  | 7.2 MiB→20 B     | ✓       | ×0.23
scrub multipart 100k | 166.0 ms | 1% | 128.2 ms | 164.7 ms | 86.6 MiB  | 7.2 MiB→7.2 MiB  | refused | 857 µs   | 6% | ≈0       | 471 µs   | ≤8.8 MiB  | 7.2 MiB→7.2 MiB  | refused | —

[footprint]
  py           memory/row (read) 896 B; memory/row (len) 601 B; disk/record 75 B; handler load 173.7 ms, rss after load 91.8 MiB, peak rss 95.6 MiB, cpu 5.45 s
  rs           memory/row (read) 372 B; memory/row (len) 203 B; disk/record 75 B; handler load —, rss after load —, peak rss —, cpu —

[summary] ratio = time against py, geometric mean per family (lower is faster)
  py           cold ×1.00  bulk ×1.00  perop ×1.00  maint ×1.00   ✗ 0  ! 0  ⏱ 0
  rs           cold ×0.12  bulk ×0.10  perop ×0.03  maint ×0.27   ✗ 0  ! 0  ⏱ 0
```

## Run 2

```
tsvzbench 1.0  2026-10-12T19:42:30-0700  suite standard  seed 1
host ksxx  cpu AMD Ryzen 9 9950X3D 16-Core Processor ×32  Linux 7.0.14-23-pve  python 3.13.5  fs f2fs  warm page cache
  py           python3 ../TSVZ/TSVZ.py  [TSVZ.py 4.5 @ 2026-10-12 by pan@zopyr.us]  handler: yes  startup 36.4 ms
  rs           target/release/tsvz  [tsvz-rs 0.1.0]  handler: no (serve exited with status 1)  startup 844 µs

[cold]
workload             | py wall  | ±  | net      | cpu      | rss       | ok | rs wall  | ±   | net      | cpu      | rss       | ok | ×
---------------------+----------+----+----------+----------+-----------+----+----------+-----+----------+----------+-----------+----+------
startup              | 36.4 ms  | 0% |          | 35.5 ms  | 34.4 MiB  | ✓  | 844 µs   | 17% |          | 561 µs   | ≤8.8 MiB  | ✓  | ×0.02
read 10k             | 55.9 ms  | 1% | 19.5 ms  | 54.9 ms  | 40.3 MiB  | ✓  | 6.1 ms   | 4%  | 5.2 ms   | 5.8 ms   | ≤8.9 MiB  | ✓  | ×0.11
len 10k              | 50.4 ms  | 0% | 14.0 ms  | 49.7 ms  | 38.3 MiB  | ✓  | 2.1 ms   | 0%  | 1.2 ms   | 1.8 ms   | ≤8.8 MiB  | ✓  | ×0.04
get 10k              | 50.3 ms  | 1% | 13.9 ms  | 49.5 ms  | 37.7 MiB  | ✓  | 2.0 ms   | 10% | 1.1 ms   | 1.5 ms   | ≤8.8 MiB  | ✓  | ×0.04
read 100k            | 230.2 ms | 0% | 193.8 ms | 228.8 ms | 118.6 MiB | ✓  | 52.3 ms  | 1%  | 51.5 ms  | 52.0 ms  | 37.1 MiB  | ✓  | ×0.23
len 100k             | 178.4 ms | 0% | 142.0 ms | 177.3 ms | 95.1 MiB  | ✓  | 14.9 ms  | 1%  | 14.0 ms  | 14.4 ms  | 19.5 MiB  | ✓  | ×0.08
get 100k             | 168.1 ms | 2% | 131.7 ms | 167.1 ms | 88.6 MiB  | ✓  | 7.2 ms   | 2%  | 6.4 ms   | 6.8 ms   | ≤8.8 MiB  | ✓  | ×0.04
read 1M              | 2.22 s   | 1% | 2.19 s   | 2.22 s   | 886.3 MiB | ✓  | 713.5 ms | 0%  | 712.6 ms | 712.6 ms | 358.7 MiB | ✓  | ×0.32
len 1M               | 1.79 s   | 1% | 1.76 s   | 1.79 s   | 605.6 MiB | ✓  | 229.6 ms | 2%  | 228.8 ms | 229.1 ms | 197.4 MiB | ✓  | ×0.13
get 1M               | 1.56 s   | 1% | 1.52 s   | 1.56 s   | 555.2 MiB | ✓  | 57.8 ms  | 1%  | 56.9 ms  | 57.4 ms  | ≤8.8 MiB  | ✓  | ×0.04
read churn 100k      | 610.4 ms | 1% | 573.9 ms | 609.0 ms | 157.2 MiB | ✓  | 139.3 ms | 2%  | 138.4 ms | 138.6 ms | 59.7 MiB  | ✓  | ×0.23
read escaped 100k    | 305.3 ms | 0% | 268.8 ms | 303.9 ms | 155.0 MiB | ✓  | 65.1 ms  | 1%  | 64.3 ms  | 64.7 ms  | 37.6 MiB  | ✓  | ×0.21
read markers 100k    | 246.7 ms | 0% | 210.3 ms | 245.6 ms | 106.6 MiB | ✓  | 44.4 ms  | 0%  | 43.5 ms  | 43.9 ms  | 29.5 MiB  | ✓  | ×0.18
read checksum 100k   | 264.9 ms | 1% | 228.5 ms | 263.6 ms | 118.6 MiB | ✓  | 52.2 ms  | 1%  | 51.4 ms  | 51.7 ms  | 36.9 MiB  | ✓  | ×0.20
read gz 100k         | 249.6 ms | 0% | 213.2 ms | 248.5 ms | 118.9 MiB | ✓  | 68.2 ms  | 0%  | 67.3 ms  | 67.6 ms  | 56.4 MiB  | ✓  | ×0.27
read multipart 100k  | 229.0 ms | 0% | 192.6 ms | 227.8 ms | 118.2 MiB | ✓  | 53.9 ms  | 1%  | 53.1 ms  | 53.4 ms  | 37.9 MiB  | ✓  | ×0.24
verify checksum 100k | 170.0 ms | 1% | 133.5 ms | 168.8 ms | 38.2 MiB  | ✓  | 19.6 ms  | 1%  | 18.7 ms  | 19.2 ms  | 19.2 MiB  | ✓  | ×0.12

[bulk]
workload       | py wall  | ±  | net      | cpu      | rss       | disk      | ok | rs wall  | ±   | net      | cpu      | rss       | disk      | ok | ×
---------------+----------+----+----------+----------+-----------+-----------+----+----------+-----+----------+----------+-----------+-----------+----+------
set - new 10k  | 52.2 ms  | 1% | 15.8 ms  | 51.2 ms  | 40.0 MiB  | 737.6 KiB | ✓  | 6.2 ms   | 4%  | 5.4 ms   | 5.9 ms   | ≤8.8 MiB  | 737.6 KiB | ✓  | ×0.12
set - new 100k | 185.8 ms | 1% | 149.4 ms | 184.6 ms | 100.3 MiB | 7.2 MiB   | ✓  | 52.7 ms  | 0%  | 51.9 ms  | 52.1 ms  | 59.9 MiB  | 7.2 MiB   | ✓  | ×0.28
set - new 1M   | 2.19 s   | 1% | 2.16 s   | 2.19 s   | 719.1 MiB | 72.0 MiB  | ✓  | 516.0 ms | 1%  | 515.1 ms | 515.2 ms | 580.9 MiB | 72.0 MiB  | ✓  | ×0.24
set - add 100k | 39.1 ms  | 1% | 2.7 ms   | 38.3 ms  | 33.8 MiB  | 7.3 MiB   | ✓  | 1.6 ms   | 11% | 728 µs   | 1.2 ms   | ≤8.8 MiB  | 7.3 MiB   | ✓  | ×0.04
delete - 100k  | 43.7 ms  | 0% | 7.2 ms   | 42.8 ms  | 34.9 MiB  | 7.3 MiB   | ✓  | 2.1 ms   | 10% | 1.2 ms   | 1.6 ms   | ≤8.8 MiB  | 7.3 MiB   | ✓  | ×0.05

[perop]
workload            | py p50   | p95      | p99      | ops/s   | rss      | ok | rs p50 | p95    | p99    | ops/s  | rss      | ok  | ×     | ceiling ops/s
--------------------+----------+----------+----------+---------+----------+----+--------+--------+--------+--------+----------+-----+-------+--------------
cli set             | 38.3 ms  | 39.8 ms  | 40.7 ms  | 26/s    | 34.3 MiB | ✓  | 633 µs | 944 µs | 1.2 ms | 1.5k/s | ≤8.8 MiB | ✓   | ×0.02 |
cli get             | 168.5 ms | 172.5 ms | 174.1 ms | 6/s     | 88.9 MiB | ✓  | 6.6 ms | 7.3 ms | 8.5 ms | 150/s  | ≤8.8 MiB | ✓   | ×0.04 |
cli via handler set | 40.7 ms  | 42.4 ms  | 42.9 ms  | 25/s    | 34.4 MiB | ✓  | —      | —      | —      | —      |          | n/a | —     |
cli via handler get | 40.2 ms  | 41.9 ms  | 42.2 ms  | 25/s    | 34.4 MiB | ✓  | —      | —      | —      | —      |          | n/a | —     |
socket get          | 19 µs    | 21 µs    | 28 µs    | 52.5k/s |          | ✓  | —      | —      | —      | —      |          | n/a | —     | 298.8k/s
socket set          | 10 µs    | 19 µs    | 25 µs    | 94.5k/s |          | ✓  | —      | —      | —      | —      |          | n/a | —     | 267.6k/s
socket --sync set   | 705 µs   | 806 µs   | 978 µs   | 1.4k/s  |          | ✓  | —      | —      | —      | —      |          | n/a | —     | 253.8k/s
socket mix ×1       | 48 µs    | 88 µs    | 102 µs   | 26.9k/s |          | ✓  | —      | —      | —      | —      |          | n/a | —     | 284.5k/s
socket mix ×4       | 134 µs   | 281 µs   | 498 µs   | 27.2k/s |          | ✓  | —      | —      | —      | —      |          | n/a | —     | 942.9k/s
socket mix ×16      | 525 µs   | 1.4 ms   | 2.1 ms   | 23.9k/s |          | ✓  | —      | —      | —      | —      |          | n/a | —     | 1.91M/s

[maint]
workload             | py wall  | ±  | net      | cpu      | rss       | disk             | ok      | rs wall  | ±   | net      | cpu      | rss       | disk             | ok      | ×
---------------------+----------+----+----------+----------+-----------+------------------+---------+----------+-----+----------+----------+-----------+------------------+---------+------
scrub churn 100k     | 717.6 ms | 1% | 681.2 ms | 713.3 ms | 157.2 MiB | 24.0 MiB→7.2 MiB | ✓       | 230.7 ms | 3%  | 229.8 ms | 228.9 ms | 106.4 MiB | 24.0 MiB→7.2 MiB | ✓       | ×0.32
scrub markers 100k   | 273.9 ms | 2% | 237.4 ms | 268.4 ms | 106.3 MiB | 4.4 MiB→7.4 MiB  | ✓       | 66.6 ms  | 1%  | 65.8 ms  | 64.2 ms  | 68.2 MiB  | 4.4 MiB→7.4 MiB  | ✓       | ×0.24
clear 100k           | 171.9 ms | 0% | 135.5 ms | 167.9 ms | 88.9 MiB  | 7.2 MiB→20 B     | ✓       | 40.2 ms  | 1%  | 39.4 ms  | 38.1 ms  | 43.2 MiB  | 7.2 MiB→20 B     | ✓       | ×0.23
scrub multipart 100k | 166.7 ms | 0% | 130.2 ms | 165.6 ms | 86.3 MiB  | 7.2 MiB→7.2 MiB  | refused | 972 µs   | 15% | 128 µs   | 615 µs   | ≤8.8 MiB  | 7.2 MiB→7.2 MiB  | refused | —

[footprint]
  py           memory/row (read) 896 B; memory/row (len) 601 B; disk/record 75 B; handler load 178.0 ms, rss after load 91.8 MiB, peak rss 95.7 MiB, cpu 5.49 s
  rs           memory/row (read) 372 B; memory/row (len) 203 B; disk/record 75 B; handler load —, rss after load —, peak rss —, cpu —

[summary] ratio = time against py, geometric mean per family (lower is faster)
  py           cold ×1.00  bulk ×1.00  perop ×1.00  maint ×1.00   ✗ 0  ! 0  ⏱ 0
  rs           cold ×0.11  bulk ×0.11  perop ×0.03  maint ×0.26   ✗ 0  ! 0  ⏱ 0
```
