#!/usr/bin/env python3
"""Real unprivileged demo builds in disposable directories; NEVER installs.

Only tests bwrap/makepkg isolation. Does not load eBPF or claim policy coverage.
The evil demo's connection is attempted only in the network-isolated Build stage.
"""
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get('ARCHON_AUDIT_BIN', ROOT / 'target/debug/archon'))
for tool in ['bwrap', 'makepkg', 'gcc', 'fakeroot', 'pacman']:
    if not shutil.which(tool):
        raise SystemExit(f'UNVERIFIED: missing {tool}; live sandbox builds cannot run')
if os.geteuid() == 0:
    raise SystemExit('UNVERIFIED: run this check as an ordinary build user, never root')

def fingerprint(path):
    if not path.exists(): return None
    return (hashlib.sha256(path.read_bytes()).hexdigest(), path.stat().st_mtime_ns)

markers = [Path('/tmp/archon-evil-marker'), Path('/var/tmp/archon-evil-marker')]
before = [fingerprint(path) for path in markers]
with tempfile.TemporaryDirectory(prefix='archon-live-sandbox-') as tmp:
    for name in ['archon-demo-benign', 'archon-demo-evil']:
        recipe = Path(tmp) / name
        recipe.mkdir()
        shutil.copyfile(ROOT / 'demo' / name / 'PKGBUILD', recipe / 'PKGBUILD')
        command = [str(BINARY), 'build', str(recipe)]
        print('$', ' '.join(command), flush=True)
        result = subprocess.run(command, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=90)
        print(result.stdout, end='')
        print('EXIT:', result.returncode, flush=True)
        assert result.returncode == 0, f'{name} real sandbox build failed'
        archives = list(recipe.glob('*.pkg.tar*'))
        assert archives and all(not p.name.endswith('.sig') for p in archives)
        print(f'PASS: real {name} sandbox build; archive metadata inspected without installation', flush=True)
assert [fingerprint(path) for path in markers] == before, 'evil demo changed a host marker'
print('PASS: evil demo did not create/change host /tmp or /var/tmp markers')
print('UNVERIFIED: eBPF flagging, root uid/gid drop and privileged installation; this check exercises none of them')
