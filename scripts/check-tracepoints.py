#!/usr/bin/env python3
"""Read-only comparison of source tracepoint reads with kernel format files.

Never elevates privileges or loads BPF. It does not verify the prebuilt object's
provenance, event delivery, ring-buffer loss, or malicious-build detection.
"""
import argparse
import hashlib
from pathlib import Path
import re
import sys
import unittest

ROOT = Path(__file__).resolve().parents[1]
READS = [('execve', 'filename_ptr', 'filename'), ('openat', 'filename_ptr', 'filename'),
         ('openat', 'flags', 'flags'), ('connect', 'sockaddr_ptr', 'uservaddr')]


def requirements():
    source = (ROOT / 'crates/archon-monitor-ebpf/src/main.rs').read_text()
    result = []
    for syscall, variable, field in READS:
        block = source.split(f'fn try_{syscall}(', 1)[1].split('\n}\n', 1)[0]
        match = re.search(r'let\s+' + variable + r':\s*u(\d+)\s*=\s*unsafe\s*\{\s*ctx\.read_at\((\d+)\)', block)
        if not match:
            raise ValueError(f'cannot identify source read for {syscall}/{variable}')
        result.append((syscall, field, int(match[2]), int(match[1]) // 8))
    return result


def fields(text):
    result = {}
    for declaration, offset, size in re.findall(r'field:([^;]+);\s*offset:\s*(\d+);\s*size:\s*(\d+);', text):
        name = declaration.split()[-1].lstrip('*')
        result[name] = (int(offset), int(size))
    return result


def matches(text, name, offset, size):
    actual = fields(text).get(name)
    return actual is not None and actual[0] == offset and actual[1] >= size


class ParserTests(unittest.TestCase):
    def test_pointer_declaration(self):
        self.assertTrue(matches('field:const char * filename; offset:16; size:8; signed:0;', 'filename', 16, 8))
    def test_wrong_offset_rejected(self):
        self.assertFalse(matches('field:const char *filename; offset:24; size:8; signed:0;', 'filename', 16, 8))
    def test_narrow_field_rejected(self):
        self.assertFalse(matches('field:int flags; offset:32; size:4; signed:1;', 'flags', 32, 8))
    def test_missing_field_rejected(self):
        self.assertFalse(matches('field:int other; offset:32; size:8; signed:1;', 'flags', 32, 8))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--tracefs', type=Path, default=Path('/sys/kernel/tracing'))
    parser.add_argument('--self-test', action='store_true')
    args = parser.parse_args()
    if args.self_test:
        suite = unittest.defaultTestLoader.loadTestsFromTestCase(ParserTests)
        raise SystemExit(not unittest.TextTestRunner(verbosity=2).run(suite).wasSuccessful())
    try:
        required = requirements()
    except (OSError, ValueError, IndexError) as error:
        raise SystemExit(f'FAIL: source-layout extraction: {error}')
    status = 0
    for syscall, field, offset, size in required:
        path = args.tracefs / f'events/syscalls/sys_enter_{syscall}/format'
        try:
            content = path.read_text()
        except OSError as error:
            print(f'UNVERIFIED: {path}: {error}')
            status = 2
            continue
        valid = matches(content, field, offset, size)
        print(f'{"PASS" if valid else "FAIL"}: sys_enter_{syscall}.{field}: '
              f'source read ({offset}, {size}), kernel field {fields(content).get(field)}')
        if not valid and status != 2: status = 1
    obj = ROOT / 'crates/archon-monitor/prebuilt/archon-monitor-ebpf.bpf.o'
    if obj.exists(): print('Prebuilt object SHA256:', hashlib.sha256(obj.read_bytes()).hexdigest())
    print('Live event capture and prebuilt/source correspondence remain UNVERIFIED by this check.')
    raise SystemExit(status)
