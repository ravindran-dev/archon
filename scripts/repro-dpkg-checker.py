#!/usr/bin/env python3
"""Ensure a dev verification tool cannot return success on comparator mismatch."""
import os
from pathlib import Path
import subprocess
import tempfile
root = Path(__file__).resolve().parents[1]
with tempfile.TemporaryDirectory(prefix='archon-dpkg-exit-') as directory:
    fake = Path(directory) / 'dpkg'
    fake.write_text('#!/bin/sh\n# Deliberately wrong comparator: everything equals.\n[ "$3" = eq ]\n')
    fake.chmod(0o755)
    env = os.environ.copy()
    env['PATH'] = directory
    result = subprocess.run([str(root / 'target/debug/verify_dpkg')], env=env, text=True, capture_output=True)
    print('$ verify_dpkg (deliberately incorrect mocked dpkg)')
    print(result.stdout, end=''); print(result.stderr, end='')
    print('exit:', result.returncode)
    assert 'mismatches found' in result.stdout
    assert result.returncode != 0, 'mismatched verification must fail, not exit successfully'
    print('PASS: mismatches produce a failing exit status')
