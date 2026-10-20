#!/usr/bin/env python3
"""Exercise actual immutable archive code and read-only native pacman parsing.
No package install/removal, privilege elevation or network access.
"""
import io
import json
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile

root = Path(__file__).resolve().parents[1]
with tempfile.TemporaryDirectory(prefix='archon-archive-snapshot-') as tmp:
    work = Path(tmp)
    package = work / 'fixture-app-1-1-any.pkg.tar'
    with tarfile.open(package, 'w') as tar:
        data = b'pkgname = fixture-app\npkgbase = fixture-app\npkgver = 1-1\npkgdesc = immutable archive fixture\nbuilddate = 1\nsize = 0\narch = any\n'
        entry = tarfile.TarInfo('.PKGINFO')
        entry.size = len(data)
        tar.addfile(entry, io.BytesIO(data))
    source = work / 'main.rs'
    source.write_text('#[path = ' + json.dumps(str(root / 'crates/archon/src/archive.rs')) + '] mod archive;\n' + r'''
use std::io::Write;
fn main() -> anyhow::Result<()> {
    let source = std::path::PathBuf::from(std::env::args().nth(1).unwrap());
    let original = std::fs::read(&source)?;
    let snapshot = archive::PackageArchive::snapshot(&source)?;
    std::fs::remove_file(&source)?;
    std::fs::write(&source, b"attacker replacement")?;
    assert_eq!(std::fs::read(snapshot.path())?, original);
    let mut writable = std::fs::OpenOptions::new().write(true).open(snapshot.path())?;
    assert!(writable.write_all(b"overwrite").is_err());
    assert!(writable.set_len(0).is_err());
    assert!(writable.set_len(original.len() as u64 + 1).is_err());
    assert_eq!(std::fs::read(snapshot.path())?, original);
    let link = source.with_extension("symlink");
    std::os::unix::fs::symlink(&source, &link)?;
    assert!(archive::PackageArchive::snapshot(&link).is_err());
    let fifo = source.with_extension("fifo");
    let name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes())?;
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    assert!(archive::PackageArchive::snapshot(&fifo).is_err());
    snapshot.verify_detached_signature()?;
    println!("PASS: original replacement, write, grow, shrink, symlink and FIFO attacks rejected");
    if std::env::var_os("ARCHON_AUDIT_NATIVE_PACMAN").is_some() {
        archive::check_handoff_policy()?;
        let output = std::process::Command::new("pacman").args(["-Qp", "--"]).arg(snapshot.path()).output()?;
        print!("{}", String::from_utf8_lossy(&output.stdout));
        if !output.status.success() { anyhow::bail!("native pacman query failed: {}", String::from_utf8_lossy(&output.stderr)); }
        assert!(String::from_utf8_lossy(&output.stdout).starts_with("fixture-app 1-1"));
        println!("PASS: real pacman parses sealed snapshot after original archive replacement");
    } else { println!("UNVERIFIED: native pacman absent"); }
    println!("Snapshot source label: {}", snapshot.source().file_name().unwrap().to_string_lossy());
    Ok(())
}
''')
    deps = root / 'target/debug/deps'
    binary = work / 'probe'
    command = ['rustc', '--edition=2024', str(source), '-L', f'dependency={deps}', '-o', str(binary)]
    for name in ['libc', 'anyhow']:
        lib = max(deps.glob('lib' + name + '-*.rlib'), key=lambda p: p.stat().st_mtime)
        command += ['--extern', f'{name}={lib}']
    subprocess.run(command, check=True)
    import os
    env = os.environ.copy()
    env.pop('ARCHON_AUDIT_NATIVE_PACMAN', None)
    if shutil.which('pacman'): env['ARCHON_AUDIT_NATIVE_PACMAN'] = '1'
    subprocess.run([str(binary), str(package)], env=env, check=True, timeout=20)
