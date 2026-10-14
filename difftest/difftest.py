#!/usr/bin/env python3
"""Differential test: random stores must read the same in tsvz-rs and in TSVZ.py.

Each seed builds a store from random lines (markers valid and invalid, tokens and bare `<`,
CRLF, zero fill, invalid UTF-8, tombstones, empty keys, checksum segments right and wrong,
a byte-order mark, a torn tail), split over one to three parts, some of them compressed
(gzip, bzip2, xz, and zstd when this Python has it) with members cut short, corrupted or, for
gzip, padded. Both tools then answer the same reads, compared byte for byte: tsvz-rs pads
rows to the store's width as TSVZ.py does (§3.6 lets a reader pad). `read`, `verify` and
`parts` are also run on a pseudo-terminal of fixed width, and their tables compared exactly
(design section 7.3), and `read` with a `-d` that the strict extension overrides (§20.7.1).
Loose files are left out: tsvz-rs reads them by design section 5.6's rule, TSVZ.py by its
3.39 rules (§20.2.4 allows both; docs/silent-choices.md).

With --scrub, each seed's store is also scrubbed by tsvz-rs, in place for one part and by a
snapshot of the parts before the active one for several (§19.2), on a copy: TSVZ.py, and
tsvz-rs, must then read the copy exactly as they read the original (§19.4), padding aside
(§3.6). A scrub that is refused (exit 1) is counted, not compared.

With --handler, each seed's store is served by `tsvz serve`, so tsvz-rs's operations go through
its handler (§20.8), while TSVZ.py still reads the files (`--x-direct`): what a handler answers
must be what the files give (§20.8.1, §21.8.5).

usage: python3 difftest/difftest.py --rust target/release/tsvz [--tsvz ../TSVZ/TSVZ.py]
                                    [--seeds 0:200] [--keep DIR] [--handler] [--scrub]
Exit status: 0 when every seed agrees, 1 when one does not (its files are kept).
"""
import argparse
import bz2
import gzip
import lzma
import os
import random
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import zlib

try:
    from compression import zstd as _zstd  # Python 3.14+
except ImportError:
    _zstd = None

CODECS = {
    'gz': lambda data: gzip.compress(data, mtime=0),
    'bz2': bz2.compress,
    'xz': lzma.compress,
}
if _zstd is not None:
    CODECS['zst'] = lambda data: _zstd.compress(data, options={_zstd.CompressionParameter.checksum_flag: 1})

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
    ('#_rotate_#', lambda r: [r.choice(['keep', 'rename', 'delete'])]),
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


def compressed_part(rng, data, suffix):
    cuts = sorted(rng.sample(range(len(data) + 1), min(2, len(data) + 1)))
    out, prev = b'', 0
    for cut in cuts + [len(data)]:
        member = CODECS[suffix](data[prev:cut])
        prev = cut
        r = rng.random()
        if r < 0.1:
            member = member[:rng.randint(10, len(member))]  # cut short
        elif r < 0.2:
            b = bytearray(member)
            b[rng.randrange(len(b))] ^= 0xff  # corrupt
            member = bytes(b)
        out += member
        if suffix == 'gz' and rng.random() < 0.1:
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
        if rng.random() < 0.3:
            suffix = rng.choice(sorted(CODECS))
            name, data = name + '.' + suffix, compressed_part(rng, data, suffix)
            if name == 's.%s.%s' % (ext, suffix):
                store = os.path.join(directory, name)  # a compressed part 0 is named by its path
        with open(os.path.join(directory, name), 'wb') as f:
            f.write(data)
    return store, d


def ask_tty(command, store, op, columns=72):
    """(exit status, output) of an operation whose stdout is a terminal `columns` wide: its table."""
    import fcntl
    import pty
    import select
    import struct
    import termios
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 24, columns, 0, 0))
    proc = subprocess.Popen(command + ['-q', op[0], store] + op[1:], stdin=subprocess.DEVNULL, stdout=slave,
                            stderr=subprocess.DEVNULL)
    os.close(slave)
    chunks = []
    while True:
        ready, _, _ = select.select([master], [], [], 60)
        if not ready:
            break
        try:
            chunk = os.read(master, 65536)
        except OSError:
            break
        if not chunk:
            break
        chunks.append(chunk)
    os.close(master)
    try:
        code = proc.wait(timeout=60)
    except subprocess.TimeoutExpired:
        proc.kill()
        return ('timeout', b'')
    return (code, b''.join(chunks).replace(b'\r\n', b'\n'))


def ask(command, store, op):
    try:
        done = subprocess.run(command + ['--format', 'records', '-q', op[0], store] + op[1:],
                              stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=60)
    except subprocess.TimeoutExpired:
        return ('timeout', b'')
    return (done.returncode, done.stdout)


def serve(command, store):
    """Start `tsvz serve STORE`; return the process once its pointer file names a socket that answers."""
    proc = subprocess.Popen(command + ['-q', 'serve', store], stdin=subprocess.DEVNULL,
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        if proc.poll() is not None:
            raise RuntimeError('serve exited with status %d' % proc.returncode)
        try:
            with open(store + '.serve', encoding='utf-8') as f:
                lines = f.read().split('\n')
            fields = dict(line.split('\t', 1) for line in lines[1:] if '\t' in line)
            address = fields.get('address', '')
            if lines[0] == 'tsvz-handler\t1' and address.startswith('unix:'):
                with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as probe:
                    probe.connect(address[5:])
                return proc
        except OSError:
            pass
        time.sleep(0.02)
    proc.kill()
    proc.wait()
    raise RuntimeError('no handler answered within 10 s')


def unserve(command, store, proc):
    """Stop the handler of STORE and reap it."""
    subprocess.run(command + ['-q', 'stop', store], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                   timeout=60)
    try:
        proc.wait(timeout=30)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait()


def trimmed(output, delimiter):
    """Output lines without the empty cells a reader pads rows with (§3.6)."""
    d = delimiter.encode()
    return [line.rstrip(d) for line in output.split(b'\n')]


def check_scrub(rust, python, store, delimiter, directory):
    """(why it was refused or None, problems) of scrubbing a copy of the store: TSVZ.py and
    tsvz-rs must read the copy as they read the original. A refusal by scrub's own fidelity
    guard is a problem: the content it built would have read differently."""
    copy = directory + '-scrubbed'
    shutil.copytree(directory, copy)
    try:
        scrubbed = os.path.join(copy, os.path.basename(store))
        done = subprocess.run(rust + ['-q', 'scrub', scrubbed], stdout=subprocess.DEVNULL,
                              stderr=subprocess.PIPE, timeout=60)
        if done.returncode != 0:
            lines = done.stderr.decode(errors='replace').strip().split('\n')
            why = lines[-1].partition('scrub skipped: ')[2] or lines[-1].replace(copy, '<store>')
            return why, ['  scrub refused by its fidelity guard: %s' % why] if 'please report' in why else []
        problems = []
        for who, command in (('TSVZ.py', python), ('tsvz-rs', rust)):
            for op in (['read'], ['len'], ['keys'], ['get', 'a', 'zz', 'tr  ', '#x', ' sp']):
                before, after = ask(command, store, op), ask(command, scrubbed, op)
                if (before[0], trimmed(before[1], delimiter)) != (after[0], trimmed(after[1], delimiter)):
                    problems.append('  scrub, then %s %s: before %s %r\n%s after %s %r' % (
                        who, ' '.join(op), before[0], before[1][:300], ' ' * (len(who) + 13), after[0], after[1][:300]))
        return None, problems
    finally:
        shutil.rmtree(copy, ignore_errors=True)


# Differences where TSVZ.py, not tsvz-rs, departs from the spec: (reason, test). Remove an entry
# once TSVZ.py is fixed. Each is listed in docs/silent-choices.md.
KNOWN = []  # none as of TSVZ.py 4.6


def known(op, rs, py):
    return next((reason for reason, test in KNOWN if test(op, rs, py)), None)


def main():
    parser = argparse.ArgumentParser(description='Differential test of tsvz-rs against TSVZ.py.')
    here = os.path.dirname(os.path.abspath(__file__))
    parser.add_argument('--rust', required=True, help='the tsvz-rs binary')
    parser.add_argument('--tsvz', default=os.path.join(here, '..', '..', 'TSVZ', 'TSVZ.py'), help='TSVZ.py')
    parser.add_argument('--seeds', default='0:200', help='START:END, end excluded')
    parser.add_argument('--keep', help='where to keep the stores of seeds that disagree (default: a temporary directory)')
    parser.add_argument('--scrub', action='store_true',
                        help='also scrub a copy of each store with tsvz-rs and check that both tools read it as before')
    parser.add_argument('--handler', action='store_true',
                        help="send tsvz-rs's operations through a tsvz-rs handler; TSVZ.py still reads the files")
    args = parser.parse_args()
    start, end = (int(x) for x in args.seeds.split(':'))
    rust = [os.path.abspath(args.rust)]
    python = [sys.executable, os.path.abspath(args.tsvz)]
    if args.handler:
        python.append('--x-direct')  # the files, not the handler that serves them
    keep = args.keep or tempfile.mkdtemp(prefix='difftest-')
    failed = 0
    refused = {}
    seen = {}
    ops = [['read'], ['len'], ['keys'], ['verify'], ['parts'], ['get', 'a', 'zz', 'tr  ', '#x', ' sp'],
           ['read', '-d', 'comma']]
    for seed in range(start, end):
        rng = random.Random(seed)
        directory = tempfile.mkdtemp(prefix='seed%d-' % seed)
        try:
            store, delimiter = make(rng, directory)
            problems = []
            handler = None
            if args.handler:
                try:
                    handler = serve(rust, store)
                except RuntimeError as e:
                    problems.append('  serve: %s' % e)
            for op in ops + [['table', 'read'], ['table', 'verify'], ['table', 'parts']]:
                if op[0] == 'table':
                    a, b = ask_tty(rust, store, op[1:]), ask_tty(python, store, op[1:])
                else:
                    a, b = ask(rust, store, op), ask(python, store, op)
                same = a == b
                if not same and known(op, a, b):
                    seen[known(op, a, b)] = seen.get(known(op, a, b), 0) + 1
                elif not same:
                    problems.append('  %s: tsvz-rs %s %r\n  %s  TSVZ.py %s %r' % (' '.join(op), a[0], a[1][:300], ' ' * len(' '.join(op)), b[0], b[1][:300]))
            if handler is not None:
                unserve(rust, store, handler)
            if args.scrub:
                why, scrub_problems = check_scrub(rust, python, store, delimiter, directory)
                if why is not None:
                    refused[why] = refused.get(why, 0) + 1
                problems += scrub_problems
            if problems:
                failed += 1
                target = os.path.join(keep, 'seed%d' % seed)
                shutil.copytree(directory, target)
                print('seed %d disagrees (files in %s):\n%s' % (seed, target, '\n'.join(problems)))
        finally:
            shutil.rmtree(directory, ignore_errors=True)
    for reason, count in seen.items():
        print('known TSVZ.py difference, %d times: %s' % (count, reason))
    if args.scrub:
        print('%d scrubs refused (not compared)' % sum(refused.values()))
        for why, count in sorted(refused.items(), key=lambda r: -r[1]):
            print('  %d: %s' % (count, why))
    print('%d of %d seeds disagree' % (failed, end - start))
    return 1 if failed else 0


if __name__ == '__main__':
    sys.exit(main())
