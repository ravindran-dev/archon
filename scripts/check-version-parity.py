#!/usr/bin/env python3
"""Compare the actual Version implementation with native vercmp/dpkg tools."""
import argparse
import itertools
from pathlib import Path
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
VERSIONS = ['1', '1.0', '1.0-1', '1.0-2', '1.0.a', '1.0a', '1.0rc1', '1.0~rc1',
            '1.0~rc2', '1.0+git', '1..0', '01', '1:1', '2:0', '1.0-1.2', '1.0-1.1',
            '1.0_1', '1.0a1', '1.0.a1', '1.0~~', '1.0~', '1.0.0', '1.0.1', '1.0.01',
            '10', '2', '1.001', '0.99', '1.0-1ubuntu1', '1.0-1+b1']
PROBE = r'''
use std::{cmp::Ordering, io::{self, BufRead}};
use archon_types::Version;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dpkg = std::env::args().nth(1).as_deref() == Some("dpkg");
    for line in io::stdin().lock().lines() {
        let line = line?;
        let parts: Vec<_> = line.split_whitespace().collect();
        if parts.len() != 2 { return Err("expected two versions".into()); }
        let parse = |text| if dpkg { Version::parse_dpkg(text) } else { Version::parse(text) };
        let ordering = parse(parts[0])?.vercmp(&parse(parts[1])?)?;
        println!("{}", match ordering { Ordering::Less => -1, Ordering::Equal => 0, Ordering::Greater => 1 });
    }
    Ok(())
}
'''
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--backend', choices=['alpm', 'dpkg', 'both'], default='both')
args = parser.parse_args()
pairs = list(itertools.product(VERSIONS, repeat=2))
status = 0
with tempfile.TemporaryDirectory(prefix='archon-vercmp-') as directory:
    source = Path(directory) / 'main.rs'
    source.write_text(PROBE)
    binary = Path(directory) / 'probe'
    library = max((ROOT / 'target/debug/deps').glob('libarchon_types-*.rlib'), key=lambda p: p.stat().st_mtime)
    subprocess.run(['rustc', '--edition=2024', str(source), '-L', f'dependency={ROOT / "target/debug/deps"}',
                    '--extern', f'archon_types={library}', '-o', str(binary)], check=True)
    for backend in ['alpm', 'dpkg'] if args.backend == 'both' else [args.backend]:
        native = shutil.which('vercmp' if backend == 'alpm' else 'dpkg')
        if not native:
            print(f'UNVERIFIED: {backend}: native comparator not installed')
            if status == 0: status = 2
            continue
        result = subprocess.run([str(binary), backend], input=''.join(f'{a} {b}\n' for a, b in pairs),
                                text=True, capture_output=True, check=True)
        values = list(map(int, result.stdout.splitlines()))
        assert len(values) == len(pairs)
        mismatches = 0
        for (a, b), ours in zip(pairs, values):
            if backend == 'alpm':
                output = subprocess.run([native, a, b], text=True, capture_output=True, check=True)
                theirs = int(output.stdout)
                theirs = (theirs > 0) - (theirs < 0)
            else:
                less = subprocess.run([native, '--compare-versions', a, 'lt', b]).returncode
                greater = subprocess.run([native, '--compare-versions', a, 'gt', b]).returncode
                if less not in [0, 1] or greater not in [0, 1]: raise SystemExit('native dpkg comparison failed')
                theirs = -1 if less == 0 else 1 if greater == 0 else 0
            if ours != theirs:
                print(f'MISMATCH {backend}: {a!r} vs {b!r}: archon={ours}, native={theirs}')
                mismatches += 1
        print(f'{"FAIL" if mismatches else "PASS"}: {backend}: {len(pairs)} comparisons, {mismatches} mismatches')
        if mismatches: status = 1
raise SystemExit(status)
