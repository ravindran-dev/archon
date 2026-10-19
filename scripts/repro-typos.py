#!/usr/bin/env python3
"""Exercise actual typo handling without a live server or host package changes."""
import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile

spec = importlib.util.spec_from_file_location('aur_regression', Path(__file__).with_name('repro-aur-install.py'))
helper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helper)
wrapper = r'''
pub fn audit_unmonitored_install() -> Result<()> {
    let official = OfficialRepos::from_dbs(vec![]);
    let aur = AurClient::new("http://127.0.0.1:0")?;
    let typo = std::env::var("FIXTURE_TYPO")?;
    handle_typo_correction(&official, &aur, &typo)?;
    Ok(())
}
'''
with tempfile.TemporaryDirectory(prefix='archon-typos-') as directory:
    root = Path(directory)
    binary = helper.compile_direct_harness(root, wrapper)
    apt = root / 'apt'
    apt.mkdir()
    (apt / 'fixture_Packages').write_text('Package: debianonly\nVersion: 1\n\n')
    failures = []
    for name, typo in [('unicode', 'éé'), ('backend-isolation', 'debianonl')]:
        env = os.environ.copy()
        env.pop('ARCHON_FORCE_INTERACTIVE', None)
        env.update(FIXTURE_TYPO=typo, ARCHON_APT_LISTS_DIR=str(apt))
        result = subprocess.run([str(binary)], env=env, stdin=subprocess.DEVNULL,
                                capture_output=True, text=True, timeout=10)
        print('$ direct-production-typo-path', repr(typo))
        print(result.stdout, end=''); print(result.stderr, end='')
        print('exit:', result.returncode)
        if result.returncode != 0 or 'debianonly' in result.stderr:
            print('FAIL:', name); failures.append(name)
        else:
            print('PASS:', name)
    if failures: raise SystemExit('Regression failures: ' + ', '.join(failures))
