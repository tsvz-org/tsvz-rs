#!/usr/bin/env python3
"""tsvzbench's `socket mix ×1` on its own: one client alternating `set k` and `get k` on a handler.

Each repetition copies FIXTURE into a fresh directory under WORK, starts `TSVZ serve` on the
copy, optionally sends tsvzbench's get, set and --sync set series first (--prelude), then times
the 5000 mix requests. tsvzbench's mix starts about 1 s after the handler wrote its pointer file
into that directory: too recent for its timestamp to be trusted (2 s). --warm S sends `len`
requests for S seconds first, so that the directory ages (S > 2: a store nothing changed in for
a while) while the handler and the CPU stay busy, without the idle CPU a --delay leaves before
the timed requests. (Dating the directory back instead would not do: a handler that saw its
timestamp too recent does not look at it again until it may be old enough.) The implementations
take turns, in reverse order every other repetition.
Pin it to one CCD for steady numbers: `taskset -c 2-7 python3 bench/mix1.py ...`.

    python3 bench/mix1.py --fixture F --work-dir W [--reps N] [--delay S] [--warm S] [--prelude] NAME=TSVZ...

FIXTURE is the 100k-row plain fixture tsvzbench generates (`fixtures/plain-100000-s1-g1/plain.tsvz`
in its work directory). Uses tsvzbench's request and client helpers (--tsvzbench, default
../TSVZ/tsvzbench.py next to this repository).
"""
import argparse
import os
import random
import shutil
import statistics
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))


def prelude(tb, rows=100000, seq=10000, sync=1000):
    """tsvzbench's get, set and --sync set series, which come before the mix on its handler."""
    rng = random.Random('prelude')
    out = [tb.request('get', (tb.key_of(rng.randrange(rows)),)) for _ in range(seq)]
    out += [tb.request('set', [tb.key_of(rng.randrange(rows))] + tb.random_values(rng)) for _ in range(seq)]
    out += [tb.request('set', [tb.key_of(rng.randrange(rows))] + tb.random_values(rng), ('--sync',))
            for _ in range(sync)]
    return out


def mix(tb, rows=100000, clients=16, per_client=5000):
    """Client 0's plan of tsvzbench's mix: set a key, then get it, over its own keys."""
    rng = random.Random('mix')
    owned = rows // clients
    keys = [tb.key_of(k) for k in range(owned)]
    out = []
    for j in range(per_client):
        key = keys[(j // 2) % owned]
        if j % 2 == 0:
            out.append(tb.request('set', [key] + tb.random_values(rng)))
        else:
            out.append(tb.request('get', (key,)))
    return out


def once(tb, args, tsvz, name, before, payloads):
    """One repetition: (p50 of all requests, of the sets, of the gets) in µs."""
    run = os.path.join(args.work_dir, name)
    shutil.rmtree(run, ignore_errors=True)
    os.makedirs(run)
    store = os.path.join(run, os.path.basename(args.fixture))
    shutil.copy2(args.fixture, store)
    with open(os.path.join(run, 'serve.err'), 'wb') as err:
        child = subprocess.Popen([tsvz, '-q', 'serve', store], stdout=subprocess.DEVNULL, stderr=err)
    stopped = False
    try:
        deadline = time.monotonic() + 30
        while True:
            pointer = tb.read_pointer(store)
            if pointer:
                probe = None
                try:
                    probe = tb.Client(pointer[0])
                    probe.roundtrip(b'len\n')
                    break
                except OSError:
                    pass
                finally:
                    if probe is not None:
                        probe.close()
            if time.monotonic() > deadline:
                sys.exit('%s: no handler answered' % name)
            time.sleep(0.005)
        time.sleep(args.delay)
        client = tb.Client(pointer[0])
        until = time.monotonic() + args.warm
        while time.monotonic() < until:
            status = tb.response_status(client.roundtrip(b'len\n'))
            if status != 0:
                sys.exit('%s: the warm-up: len answered status %d' % (name, status))
        _, responses, _, _ = tb.client_loop(client, before)
        problem = tb.check_responses(responses, [None] * len(responses))
        if problem:
            sys.exit('%s: the prelude: %s' % (name, problem))
        latencies, responses, _, _ = tb.client_loop(client, payloads)
        problem = tb.check_responses(responses, [None] * len(responses))
        client.roundtrip(b'stop\n')
        stopped = True
        client.close()
        if problem:
            sys.exit('%s: %s' % (name, problem))
    finally:
        if not stopped:
            child.kill()  # an early exit: the handler would hold the store and a CPU
        try:
            child.wait(10)
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait()
    median = lambda xs: statistics.median(xs) * 1e6
    return median(latencies), median(latencies[0::2]), median(latencies[1::2])


def main():
    parser = argparse.ArgumentParser(description="tsvzbench's socket mix ×1 on its own")
    parser.add_argument('--fixture', required=True)
    parser.add_argument('--work-dir', required=True)
    parser.add_argument('--reps', type=int, default=8)
    parser.add_argument('--delay', type=float, default=0.0, help='seconds between load and requests')
    parser.add_argument('--warm', type=float, default=0.0,
                        help='seconds of len requests before the timed ones, the directory ageing meanwhile')
    parser.add_argument('--prelude', action='store_true', help="send tsvzbench's earlier series first")
    parser.add_argument('--tsvzbench', default=os.path.join(HERE, '..', '..', 'TSVZ', 'tsvzbench.py'))
    parser.add_argument('impls', nargs='+', metavar='NAME=TSVZ')
    args = parser.parse_args()
    sys.path.insert(0, os.path.dirname(os.path.abspath(args.tsvzbench)))
    import tsvzbench as tb

    impls = [spec.split('=', 1) for spec in args.impls]
    for spec, impl in zip(args.impls, impls):
        # NAME is a directory under --work-dir, removed before each repetition.
        if len(impl) != 2 or not impl[0] or os.sep in impl[0] or impl[0] in ('.', '..'):
            parser.error('%r: expected NAME=TSVZ, NAME a plain file name' % spec)
    if len({name for name, _ in impls}) != len(impls):
        parser.error('each NAME must be another: their results would mix')
    before = prelude(tb) if args.prelude else []
    payloads = mix(tb)
    results = {name: [] for name, _ in impls}
    for rep in range(args.reps):
        for name, tsvz in (impls if rep % 2 == 0 else impls[::-1]):
            results[name].append(once(tb, args, tsvz, name, before, payloads))
    for name, rows in results.items():
        p50 = lambda i: statistics.median(row[i] for row in rows)
        print('%-10s p50 %.1f µs  set %.1f  get %.1f   per repetition %s' % (
            name, p50(0), p50(1), p50(2), ' '.join('%.1f' % row[0] for row in rows)))


if __name__ == '__main__':
    main()
