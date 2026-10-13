#!/usr/bin/env python3
"""Differential test: random stores must read the same in tsvz-rs and in TSVZ.py.

Each seed builds a store from random lines (markers valid and invalid, tokens and bare `<`,
CRLF, zero fill, invalid UTF-8, tombstones, empty keys, checksum segments right and wrong,
a byte-order mark, a torn tail), split over one to three parts, some of them gzip parts with
members cut short, corrupted or padded. Both tools then answer the same reads; rows are
compared with trailing empty cells removed (§3.6 lets a reader pad), everything else exactly.

usage: python3 difftest/difftest.py --rust target/release/tsvz [--tsvz ../TSVZ/TSVZ.py]
                                    [--seeds 0:200] [--keep DIR]
Exit status: 0 when every seed agrees, 1 when one does not (its files are kept).
"""
import argparse
import gzip
import os
import random
import shutil
import subprocess
import sys
import tempfile
import zlib

DELIMITERS = {'tsvz': '\t', 'csvz': ',', 'psvz': '|', 'nsvz': '\0'}
TOKENS = ['<sep>', '<LF>', '<CR>', '<NUL>', '<lt>', '<#>', '<TAB>', '<', '<lf>']
KEYS = ['a', 'b', 'c', 'k1', 'k2', '<#>x', ' sp', 'tr ', 'ü', '']
MARKERS = [
    ('#_defaults_#', lambda r: [cell(r) for _ in range(r.randint(0, 3))]),
    ('#_strip_trailing_whites_#', lambda r: [r.choice(['true', 'false', 'maybe', ''])]),
    ('#_fill_empty_with_default_#', lambda r: [r.choice(['true', 'false', 'yes', 'no'])]),
    ('#_return_defaults_when_missing_#', lambda r: [r.choice(['true', 'false'])]),
    ('#_version_#', lambda r: [r.choice(['1', '2', '0', 'x'])]),
    ('#_on_corruption_#', lambda r: [r.choice(['recover', 'skip', 'stop', 'recover'])]),
    ('#__ext__#', lambda r: ['v']),
    ('#_DEFAULTS_#', lambda r: ['up']),
]


def cell(rng):
    parts = [rng.choice(['x', 'yy', 'ü', ' ', '', '#', '|', ',']) for _ in range(rng.randint(0, 3))]
    if rng.random() < 0.3:
        parts.insert(rng.randint(0, len(parts)), rng.choice(TOKENS))
    text = ''.join(parts)
    return text + '  ' if rng.random() < 0.1 else text


def line(rng, d):
    r = rng.random()
    if r < 0.08:
        return rng.choice(['#comment', '#name' + d + 'age', '#'])
    if r < 0.22:
        key, values = rng.choice(MARKERS)
        return d.join([key] + values(rng))
    key = rng.choice(KEYS)
    if rng.random() < 0.15:
        return key
    return d.join([key] + [cell(rng) for _ in range(rng.randint(1, 3))])


def body(rng, d, n, checksums):
    lines = [line(rng, d).encode('utf-8') for _ in range(n)]
    out = []
    for raw in lines:
        r = rng.random()
        if r < 0.03:
            raw = b'\0\0' + raw  # zero fill
        elif r < 0.05:
            raw += b'\xff\xfe'  # invalid UTF-8
        out.append(raw + (b'\r\n' if rng.random() < 0.1 else b'\n'))
    if checksums and out:
        # crc32 segments: the first marker arms; later ones carry the right digest, or a wrong one.
        marks = sorted(rng.sample(range(len(out) + 1), min(3, len(out) + 1)))
        with_marks, segment, armed = [], b'', False
        for i, raw in enumerate(out + [None]):
            if i in marks:
                value = 'ignored'
                if armed:
                    value = '%08x' % (zlib.crc32(segment) & 0xffffffff) if rng.random() < 0.7 else '00000000'
                with_marks.append(('#_checksum_crc32_#' + d + value + '\n').encode())
                segment, armed = b'', True
            if raw is not None:
                with_marks.append(raw)
                segment += raw
        out = with_marks
    data = b''.join(out)
    if rng.random() < 0.05:
        data = b'\xef\xbb\xbf' + data
    if rng.random() < 0.2:
        data += b'torn' + d.encode()
    return data


def gzip_part(rng, data):
    cuts = sorted(rng.sample(range(len(data) + 1), min(2, len(data) + 1)))
    out, prev = b'', 0
    for cut in cuts + [len(data)]:
        member = gzip.compress(data[prev:cut], mtime=0)
        prev = cut
        r = rng.random()
        if r < 0.1:
            member = member[:rng.randint(10, len(member))]  # cut short
        elif r < 0.2:
            b = bytearray(member)
            b[rng.randrange(len(b))] ^= 0xff  # corrupt
            member = bytes(b)
        out += member
        if rng.random() < 0.1:
            out += b'\0' * rng.randint(1, 20)  # padding
    return out


def make(rng, directory):
    ext = rng.choice(['tsvz', 'tsvz', 'csvz', 'psvz', 'nsvz'])
    d = DELIMITERS[ext]
    store = os.path.join(directory, 's.' + ext)
    count = rng.choice([1, 1, 1, 2, 3])
    names = ([] if rng.random() < 0.3 else ['s.' + ext])
    names += ['s.%s.%s' % (ext, o) for o in rng.sample(['1', 'a', 'B', '10', '002', 'ff'], count)]
    for name in names[:count]:
        data = body(rng, d, rng.randint(0, 25), checksums=count == 1)
        if rng.random() < 0.25:
            name, data = name + '.gz', gzip_part(rng, data)
        with open(os.path.join(directory, name), 'wb') as f:
            f.write(data)
    return store, d


def ask(command, store, op):
    try:
        done = subprocess.run(command + ['--format', 'records', '-q', op[0], store] + op[1:],
                              stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=60)
    except subprocess.TimeoutExpired:
        return ('timeout', b'')
    return (done.returncode, done.stdout)


# Differences where TSVZ.py, not tsvz-rs, departs from the spec: (reason, test). Remove an entry
# once TSVZ.py is fixed. Each is listed in docs/silent-choices.md.
KNOWN = [
    ('TSVZ.py exits 0 from len and keys after a read stopped at damaged data (§4.10, §20.6 say 1)',
     lambda op, rs, py: op[0] in ('len', 'keys') and rs == (1, py[1]) and py[0] == 0),
]


def known(op, rs, py):
    return next((reason for reason, test in KNOWN if test(op, rs, py)), None)


def rows(out, d):
    """Records with trailing empty cells removed (§3.6)."""
    return [line.rstrip(d.encode()) for line in out.split(b'\n')]


def main():
    parser = argparse.ArgumentParser(description='Differential test of tsvz-rs against TSVZ.py.')
    here = os.path.dirname(os.path.abspath(__file__))
    parser.add_argument('--rust', required=True, help='the tsvz-rs binary')
    parser.add_argument('--tsvz', default=os.path.join(here, '..', '..', 'TSVZ', 'TSVZ.py'), help='TSVZ.py')
    parser.add_argument('--seeds', default='0:200', help='START:END, end excluded')
    parser.add_argument('--keep', help='where to keep the stores of seeds that disagree (default: a temporary directory)')
    args = parser.parse_args()
    start, end = (int(x) for x in args.seeds.split(':'))
    rust = [os.path.abspath(args.rust)]
    python = [sys.executable, os.path.abspath(args.tsvz)]
    keep = args.keep or tempfile.mkdtemp(prefix='difftest-')
    failed = 0
    seen = {}
    ops = [['read'], ['len'], ['keys'], ['verify'], ['parts'], ['get', 'a', 'zz', 'tr  ', '#x', ' sp']]
    for seed in range(start, end):
        rng = random.Random(seed)
        directory = tempfile.mkdtemp(prefix='seed%d-' % seed)
        try:
            store, d = make(rng, directory)
            problems = []
            for op in ops:
                a, b = ask(rust, store, op), ask(python, store, op)
                same = a[0] == b[0] and (rows(a[1], d) == rows(b[1], d) if op[0] in ('read', 'get') else a[1] == b[1])
                if not same and known(op, a, b):
                    seen[known(op, a, b)] = seen.get(known(op, a, b), 0) + 1
                elif not same:
                    problems.append('  %s: tsvz-rs %s %r\n  %s  TSVZ.py %s %r' % (' '.join(op), a[0], a[1][:300], ' ' * len(' '.join(op)), b[0], b[1][:300]))
            if problems:
                failed += 1
                target = os.path.join(keep, 'seed%d' % seed)
                shutil.copytree(directory, target)
                print('seed %d disagrees (files in %s):\n%s' % (seed, target, '\n'.join(problems)))
        finally:
            shutil.rmtree(directory, ignore_errors=True)
    for reason, count in seen.items():
        print('known TSVZ.py difference, %d times: %s' % (count, reason))
    print('%d of %d seeds disagree' % (failed, end - start))
    return 1 if failed else 0


if __name__ == '__main__':
    sys.exit(main())
