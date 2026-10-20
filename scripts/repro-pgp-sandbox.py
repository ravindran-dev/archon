#!/usr/bin/env python3
"""Production PGP helper regression; fake bwrap/GPG never touch host keyrings."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

root = Path(__file__).resolve().parents[1]
with tempfile.TemporaryDirectory(prefix='archon-pgp-sandbox-') as tmp:
    work = Path(tmp)
    tools = work / 'bin'
    tools.mkdir()
    calls = work / 'calls.jsonl'
    (tools / 'bwrap').write_text("""#!/usr/bin/python3
import json, os, sys
if sys.argv[1:] == ['--version']: sys.exit(0)
with open(os.environ['FIXTURE_CALLS'], 'a') as f: f.write(json.dumps(sys.argv[1:]) + '\\n')
""")
    (tools / 'gpg').write_text('#!/bin/sh\necho HOST_GPG_EXECUTED >&2\nexit 99\n')
    for tool in tools.iterdir(): tool.chmod(0o755)
    source = root / 'crates/archon/src/pgp.rs'
    driver = work / 'main.rs'
    driver.write_text('#[path = ' + json.dumps(str(source)) + '] mod pgp;\nfn main() { pgp::import_build_keys(std::path::Path::new(&std::env::args().nth(1).unwrap())); }')
    deps = root / 'target/debug/deps'
    lib = max(deps.glob('libarchon_sandbox-*.rlib'), key=lambda p: p.stat().st_mtime)
    binary = work / 'probe'
    subprocess.run(['rustc', '--edition=2024', str(driver), '-L', f'dependency={deps}', '--extern', f'archon_sandbox={lib}', '-o', str(binary)], check=True)
    package = work / 'package'
    package.mkdir()
    fingerprint = 'E1096BCBFF6D418796DE78515384CE82BA52C83A'
    (package / 'PKGBUILD').write_text("validpgpkeys=('" + fingerprint + "' '--homedir=/outside' '../outside')\n")
    live = sys.argv[1:] == ['--live']
    if live:
        bundle = package / 'keys/pgp'
        bundle.mkdir(parents=True)
        (bundle / (fingerprint + '.asc')).write_text('intentionally invalid offline public-key fixture')
        (package / '.gnupg').mkdir(mode=0o700)
        (package / '.gnupg/gpg.conf').write_text('archon-audit-invalid-option-that-must-not-be-read\n')
    env = dict(os.environ, PATH=os.environ['PATH'] if live else str(tools), FIXTURE_CALLS=str(calls))
    result = subprocess.run([str(binary), str(package)], env=env, capture_output=True, text=True, check=True)
    print(result.stderr, end='')
    if live:
        assert 'no valid OpenPGP data found' in result.stderr, 'real GPG must parse the bundled offline fixture inside bwrap'
        assert 'invalid option' not in result.stderr, 'package-controlled gpg.conf must not be read'
        print('PASS: real bwrap/GPG parsed offline bundled data and ignored package-controlled gpg.conf')
        print('UNVERIFIED: successful signature verification and privileged uid/gid switching')
        raise SystemExit(0)
    assert calls.exists(), 'PGP preparation must execute through the sandbox'
    args = json.loads(calls.read_text())
    command = args[args.index('--') + 1:]
    assert command[:2] == ['/bin/sh', '-c']
    assert command[4:] == ['keyserver.ubuntu.com', fingerprint], command
    assert '--no-options' in command[2] and '--recv-keys -- "$key"' in command[2]
    assert '--ro-bind' in args and '--cap-drop' in args and '--clearenv' in args
    assert args[args.index('--chdir') + 1] == str(package)
    assert not (package / '.gnupg').exists(), 'host helper must not create/chmod a keyring'
    assert 'HOST_GPG_EXECUTED' not in result.stderr
    print('PASS: key import is sandboxed; invalid fingerprints excluded; host keyring untouched')
