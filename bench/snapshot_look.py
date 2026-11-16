#!/usr/bin/env python3
"""Time `tsvz get` on stores of many compressed parts, to see what the look at each part for a
snapshot marker (§19.10.1) costs: `snapshot_look.py BIN...` makes the stores in the current
directory (200 gzip parts of 12,000 rows, 40 gzip parts and 40 bzip2 parts of 50,000 rows) and
prints the best of 5 runs of each binary on each. Compare a build with another, or with 0.1.3,
which looked at no part."""
import gzip, bz2, os, random, subprocess, sys, time

r = random.Random(1)


def rows(a, b):
    return ''.join('key%07d\t%016x\t%016x\tsome text here\n' % (i, r.getrandbits(64), r.getrandbits(64))
                   for i in range(a, b))


def make(dirname, parts, per_part, ext, compress):
    os.mkdir(dirname)
    for p in range(parts):
        name = 'm.tsvz.%s' % ext if p == 0 else 'm.tsvz.%d.%s' % (p, ext)
        with open(os.path.join(dirname, name), 'wb') as f:
            f.write(compress(rows(p * per_part, (p + 1) * per_part).encode()))


def best(argv, runs=5):
    times = []
    for _ in range(runs):
        t = time.time()
        subprocess.run(argv, stdout=subprocess.DEVNULL)
        times.append(time.time() - t)
    return min(times)


make('gz200', 200, 12000, 'gz', lambda b: gzip.compress(b, 6))
make('gz40', 40, 50000, 'gz', lambda b: gzip.compress(b, 6))
make('bz40', 40, 50000, 'bz2', lambda b: bz2.compress(b, 9))
for what, store in (('200 gzip parts', 'gz200/m.tsvz.gz'), ('40 gzip parts', 'gz40/m.tsvz.gz'),
                    ('40 bzip2 parts', 'bz40/m.tsvz.bz2')):
    print('%-16s' % what + '  '.join('%s %.3f s' % (b, best([b, 'get', store, 'key0000009']))
                                    for b in sys.argv[1:]), flush=True)
