#!/usr/bin/env python3
"""Read-only-to-host-files PGP probe: all mutations confined to a temp fixture.

Invokes the actual production helper as the current user, with a fake gpg.
Exit 1 means the symlink target was modified; no root or network is used.
"""
import os
import json
from pathlib import Path
import stat
import subprocess
import tempfile

root = Path(__file__).resolve().parents[1]
with tempfile.TemporaryDirectory(prefix='archon-pgp-audit-') as tmp:
    work = Path(tmp)
    package = work / 'package'
    package.mkdir()
    target = work / 'outside-package'
    target.mkdir(mode=0o755)
    target.chmod(0o755)
    (package / '.gnupg').symlink_to(target, target_is_directory=True)
    (package / 'PKGBUILD').write_text("validpgpkeys=('E1096BCBFF6D418796DE78515384CE82BA52C83A')\n")
    tools = work / 'bin'
    tools.mkdir()
    (tools / 'gpg').write_text('#!/bin/sh\necho host-gpg >> "$PGP_AUDIT_LOG"\nexit 0\n')
    (tools / 'bwrap').write_text('#!/bin/sh\nif [ "$1" = --version ]; then exit 0; fi\necho sandbox >> "$PGP_AUDIT_LOG"\nexit 0\n')
    (tools / 'bwrap').chmod(0o755)
    (tools / 'gpg').chmod(0o755)
    driver = work / 'driver.rs'
    driver.write_text('#[path = ' + repr(str(root / 'crates/archon/src/pgp.rs')).replace("'", '"') + ']\nmod pgp;\nfn main() { pgp::import_build_keys(std::path::Path::new(&std::env::args().nth(1).unwrap())); }\n')
    binary = work / 'probe'
    command = ['rustc', '--edition=2024', str(driver), '-o', str(binary), '-L', 'dependency=' + str(root / 'target/debug/deps')]
    for name in ['archon_sandbox']:
        libraries = list((root / 'target/debug/deps').glob('lib' + name + '-*.rlib'))
        command += ['--extern', name + '=' + str(max(libraries, key=lambda p: p.stat().st_mtime))]
    subprocess.run(command, check=True)
    before = stat.S_IMODE(target.stat().st_mode)
    env = dict(os.environ, PATH=str(tools) + os.pathsep + os.environ['PATH'], PGP_AUDIT_LOG=str(work / 'calls'))
    subprocess.run([str(binary), str(package)], env=env, check=True)
    after = stat.S_IMODE(target.stat().st_mode)
    print(f'Outside fixture directory mode BEFORE: {before:04o}')
    print(f'Outside fixture directory mode AFTER:  {after:04o}')
    if before != after:
        print('CONFIRMED: import_build_keys follows .gnupg symlink outside the package tree.')
        print('UNVERIFIED: root impact; this probe runs only as the current unprivileged user.')
        raise SystemExit(1)
    calls = (work / 'calls').read_text() if (work / 'calls').exists() else ''
    assert 'host-gpg' not in calls, 'untrusted key import must never run directly on the host'
    print('PASS: outside fixture directory unchanged; no host GPG invocation')
