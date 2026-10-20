#!/usr/bin/env python3
"""Hermetic CLI regression: fake AUR/build/sudo, never install host packages."""
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ROOT = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get('ARCHON_AUDIT_BIN', ROOT / 'target/debug/archon'))
DIRECT = False

class Aur(BaseHTTPRequestHandler):
    def do_GET(self):
        body = json.dumps({'version': 5, 'type': 'multiinfo', 'resultcount': 1,
                           'results': [{'Name': 'fixture-app', 'PackageBase': 'fixture-app',
                                        'Version': '1-1', 'Depends': [], 'MakeDepends': []}]}).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *_):
        pass


def run_case(case):
    with tempfile.TemporaryDirectory(prefix='archon-aur-install-') as tmp:
        work = Path(tmp)
        tools = work / 'bin'
        tools.mkdir()
        log = work / 'commands.jsonl'
        # Only mocked sudo is reachable from the test; it never executes argv.
        sudo = tools / 'sudo'
        sudo.write_text('''#!/usr/bin/python3
import fcntl, json, os, pathlib, sys
with open(os.environ['FIXTURE_LOG'], 'a') as f:
    f.write(json.dumps(sys.argv[1:]) + '\\n')
if sys.argv[1:4] != ['pacman', '-U', '--needed']:
    sys.exit('unexpected privileged command in fixture')
payloads = [json.loads(pathlib.Path(p).read_text()) for p in sys.argv[5:]]
for path in sys.argv[5:]:
    with open(path, 'rb') as archive:
        seals = fcntl.fcntl(archive, fcntl.F_GET_SEALS)
        expected = fcntl.F_SEAL_WRITE | fcntl.F_SEAL_GROW | fcntl.F_SEAL_SHRINK | fcntl.F_SEAL_SEAL
        assert seals & expected == expected, 'pacman must receive an immutable archive'
with open(os.environ['FIXTURE_LOG'] + '.payloads', 'a') as f:
    f.write(json.dumps(payloads) + '\\n')
if os.environ['FIXTURE_CASE'] == 'dependency-chain' and payloads[0]['name'] == 'z-lib':
    open(os.environ['FIXTURE_LOG'] + '.dependency-installed', 'w').close()
sys.exit(23 if os.environ['FIXTURE_CASE'] == 'install-failure' else 0)
''')
        sudo.chmod(0o755)
        bwrap = tools / 'bwrap'
        bwrap.write_text('''#!/usr/bin/python3
import json, os, pathlib, sys
if '--version' in sys.argv:
    print('bubblewrap fixture'); sys.exit(0)
if '--noextract' in sys.argv:
    if os.environ['FIXTURE_CASE'] == 'build-failure': sys.exit(7)
    if os.environ['FIXTURE_CASE'] != 'missing-artifact':
        dest = pathlib.Path(sys.argv[sys.argv.index('--chdir') + 1])
        if os.environ['FIXTURE_CASE'] == 'dependency-chain' and dest.name == 'fixture-app':
            if not pathlib.Path(os.environ['FIXTURE_LOG'] + '.dependency-installed').exists():
                sys.exit('AUR dependency was not installed before dependent build')
        artifact = dest / (dest.name + '-1-1-x86_64.pkg.tar.zst')
        artifact.write_text(json.dumps({'name': dest.name}))
        if os.environ['FIXTURE_CASE'] == 'split-base':
            for name in ['fixture-app', 'fixture-lib']:
                (dest / (name + '-1-1-x86_64.pkg.tar.zst')).write_text(json.dumps({'name': name}))
            with open(os.environ['FIXTURE_LOG'] + '.builds', 'a') as f:
                f.write('built\\n')
        if os.environ['FIXTURE_CASE'] == 'archive-identity':
            artifact.unlink()
            (dest / 'fixture-app-addon-1-1-x86_64.pkg.tar.zst').write_text(json.dumps({'name': 'fixture-app-addon'}))
        if os.environ['FIXTURE_CASE'] in ('signed-artifact', 'invalid-signature'):
            pathlib.Path(str(artifact) + '.sig').write_bytes(b'fixture signature')
        if os.environ['FIXTURE_CASE'] == 'ambiguous-artifact':
            (dest / (dest.name + '-0-1-x86_64.pkg.tar.zst')).write_text(json.dumps({'name': dest.name}))
''')
        bwrap.chmod(0o755)
        pacman = tools / 'pacman'
        pacman.write_text("""#!/usr/bin/python3
import json, os, pathlib, sys
if sys.argv[1:3] != ['-Qp', '--']: sys.exit('unexpected pacman invocation')
print(json.loads(pathlib.Path(sys.argv[-1]).read_text())['name'] + ' 1-1')
if os.environ['FIXTURE_CASE'] == 'archive-race':
    origin = pathlib.Path(os.environ['XDG_CACHE_HOME']) / 'archon/build/fixture-app/fixture-app-1-1-x86_64.pkg.tar.zst'
    origin.write_text(json.dumps({'name': 'attacker-replacement'}))
""")
        pacman.chmod(0o755)
        key = tools / 'pacman-key'
        key.write_text('''#!/usr/bin/python3
import fcntl, json, os, pathlib, sys
assert sys.argv[1:3] == ['--verify', '--']
assert pathlib.Path(sys.argv[3]).read_bytes() == b'fixture signature'
assert json.loads(pathlib.Path(sys.argv[4]).read_text())['name'] == 'fixture-app'
with open(os.environ['FIXTURE_LOG'] + '.signature', 'w') as f: f.write('verified')
sys.exit(1 if os.environ['FIXTURE_CASE'] == 'invalid-signature' else 0)
''')
        key.chmod(0o755)
        config = tools / 'pacman-conf'
        config.write_text('''#!/usr/bin/python3
import os
print('PackageRequired' if os.environ['FIXTURE_CASE'] == 'signature-required' else 'PackageOptional')
print('PackageTrustedOnly')
''')
        config.chmod(0o755)
        sync = work / 'sync'
        sync.mkdir()
        with tarfile.open(sync / 'core.db', 'w') as archive:
            info = tarfile.TarInfo('empty/')
            info.type = tarfile.DIRTYPE
            archive.addfile(info)
            if case == 'dry-run':
                import io
                desc = b'%NAME%\nfixture-app\n\n%VERSION%\n1-1\n\n'
                entry = tarfile.TarInfo('fixture-app-1-1/desc')
                entry.size = len(desc)
                archive.addfile(entry, io.BytesIO(desc))
        local = work / 'local'
        local.mkdir()
        recipe = work / 'cache/archon/build/fixture-app'
        recipe.mkdir(parents=True)
        (recipe / 'PKGBUILD').write_text('pkgname=fixture-app\n')
        if case == 'split-base':
            for name in ['suite', 'fixture-lib']:
                extra = recipe.parent / name
                extra.mkdir()
                (extra / 'PKGBUILD').write_text('pkgname=fixture\n')
        if case == 'dependency-chain':
            dependency_recipe = recipe.parent / 'z-lib'
            dependency_recipe.mkdir()
            (dependency_recipe / 'PKGBUILD').write_text('pkgname=z-lib\n')
        osrelease = work / 'os-release'
        osrelease.write_text('ID=arch\n')
        server = None
        if not DIRECT and case != 'dry-run':
            server = ThreadingHTTPServer(('127.0.0.1', 0), Aur)
            worker = threading.Thread(target=server.serve_forever, daemon=True)
            worker.start()
        env = os.environ.copy()
        env.update(PATH=str(tools), HOME=str(work), XDG_CACHE_HOME=str(work / 'cache'),
                   ARCHON_OS_RELEASE=str(osrelease), ARCHON_SYNC_DIR=str(sync),
                   ARCHON_LOCAL_DIR=str(local), ARCHON_CACHE_DIR=str(work / 'cache/archon'),
                   ARCHON_AUR_URL=f'http://127.0.0.1:{server.server_port if server else 1}',
                   FIXTURE_LOG=str(log), FIXTURE_CASE=case, NO_COLOR='1')
        args = [str(BINARY), 'install', 'fixture-app', '--yes']
        if case == 'dry-run': args.append('--dry-run')
        try:
            result = subprocess.run(args, env=env, text=True, capture_output=True, timeout=15)
        finally:
            if server:
                server.shutdown()
                server.server_close()
        calls = [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []
        print(f'$ {"direct-production-path" if DIRECT else "archon"} install fixture-app --yes' + (' --dry-run' if case == 'dry-run' else ''))
        print(f'case: {case}')
        print(result.stdout, end='')
        print(result.stderr, end='')
        print('exit:', result.returncode)
        print('mock privileged argv:', calls)
        payloads = [json.loads(line) for line in Path(str(log) + '.payloads').read_text().splitlines()] if Path(str(log) + '.payloads').exists() else []
        if payloads: print('installed fixture payloads:', payloads)
        if case in ('success', 'signed-artifact'):
            assert result.returncode == 0, 'install must succeed'
            assert len(calls) == 1, 'successful AUR install must invoke pacman -U exactly once'
            assert calls[0][:4] == ['pacman', '-U', '--needed', '--']
            assert len(calls[0]) == 5 and calls[0][4].startswith('/proc/')
            assert payloads == [[{'name': 'fixture-app'}]]
            assert 'Not installed' not in result.stdout
            if case == 'signed-artifact':
                assert Path(str(log) + '.signature').exists(), 'detached signature must be verified against the immutable package'
        elif case == 'archive-race':
            payloads = [json.loads(line) for line in Path(str(log) + '.payloads').read_text().splitlines()]
            print('installed fixture payloads:', payloads)
            assert result.returncode == 0 and payloads == [[{'name': 'fixture-app'}]], 'validated archive was replaced before installation'
        elif case == 'split-base':
            assert result.returncode == 0
            assert len(calls) == 1 and len(calls[0]) == 6, 'split outputs must install together'
            assert (work / 'commands.jsonl.builds').read_text().splitlines() == ['built'], 'one recipe build per base'
            assert all(path.startswith('/proc/') for path in calls[0][4:])
            assert len(set(calls[0][4:])) == 2
            assert sorted(p['name'] for p in payloads[0]) == ['fixture-app', 'fixture-lib']
        elif case == 'dependency-chain':
            assert result.returncode == 0 and len(calls) == 2
            assert payloads == [[{'name': 'z-lib'}], [{'name': 'fixture-app'}]]
        elif case == 'install-failure':
            assert len(calls) == 1 and result.returncode != 0, 'pacman failure must propagate'
        elif case == 'dry-run':
            assert result.returncode == 0 and not calls, 'dry run must not install'
            assert not list(recipe.glob('*.pkg.tar*')), 'dry run must not build'
        else:
            if case == 'signature-required':
                assert 'LocalFileSigLevel' in result.stderr and not list(recipe.glob('*.pkg.tar*')), 'unsupported signature policy must fail before building'
            assert result.returncode != 0 and not calls, 'failed build/missing output must not install'
        print(f'PASS: {case}')

def compile_direct_harness(directory, entry_wrapper=None):
    """Copy the production module unchanged, append only a private-function caller.

    Other CLI modules and dependencies are the actual source/compiled rlibs.
    This verifies the production path, not clap parsing or RPC dispatch.
    """
    import tomllib
    directory = Path(directory)
    src = ROOT / 'crates/archon/src'
    production = (src / 'commands/install.rs').read_text()
    wrapper = r"""
pub fn audit_unmonitored_install() -> Result<()> {
    let aur = AurClient::new("http://127.0.0.1:1")?;
    let pkg = PlannedPackage {
        meta: archon_types::PackageMeta {
            name: "fixture-app".into(),
            version: archon_types::Version::parse("1-1")?,
            description: None,
            source: archon_types::PackageSource::Aur,
            package_base: Some("fixture-app".into()),
            depends: vec![], make_depends: vec![], conflicts: vec![], provides: vec![],
        },
        required_by: vec![],
    };
    if std::env::var("FIXTURE_CASE").as_deref() == Ok("split-base") {
        let mut app = pkg;
        app.meta.package_base = Some("suite".into());
        let mut library = app.clone();
        library.meta.name = "fixture-lib".into();
        build_aur_packages(&aur, &[library, app])
    } else if std::env::var("FIXTURE_CASE").as_deref() == Ok("dependency-chain") {
        let mut dependency = pkg.clone();
        dependency.meta.name = "z-lib".into();
        dependency.meta.package_base = Some("z-lib".into());
        build_aur_packages(&aur, &[dependency, pkg])
    } else {
        build_aur_packages(&aur, &[pkg])
    }
}
"""
    if entry_wrapper is not None:
        wrapper = entry_wrapper
    (directory / 'install.rs').write_text(production + wrapper)
    modules = ''.join(f'#[path = {json.dumps(str(src / (name + ".rs")))}] mod {name};\n'
                      for name in ['archive', 'color', 'correction', 'pgp', 'search', 'spinner', 'srcinfo', 'ui'])
    modules += ('mod commands {\n'
                f'#[path = {json.dumps(str(directory / "install.rs"))}] pub mod install;\n'
                f'#[path = {json.dumps(str(src / "commands/subprocess.rs"))}] pub mod subprocess;\n'
                '}\nfn main() -> anyhow::Result<()> { commands::install::audit_unmonitored_install() }\n')
    (directory / 'main.rs').write_text(modules)
    deps = ROOT / 'target/debug/deps'
    manifest = tomllib.loads((ROOT / 'crates/archon/Cargo.toml').read_text())
    output = directory / 'direct-install'
    command = ['rustc', '--edition=2024', '-Adead_code', str(directory / 'main.rs'),
               '-L', f'dependency={deps}', '-o', str(output)]
    for dependency in manifest['dependencies']:
        name = dependency.replace('-', '_')
        libraries = sorted(deps.glob(f'lib{name}-*.rlib'), key=lambda p: p.stat().st_mtime)
        if not libraries:
            raise RuntimeError(f'Build archon first; compiled dependency missing: {name}')
        command += ['--extern', f'{name}={libraries[-1]}']
    subprocess.run(command, check=True)
    return output

if __name__ == '__main__':
    import sys
    scenarios = sys.argv[1:]
    DIRECT = '--direct' in scenarios
    if DIRECT: scenarios.remove('--direct')
    if DIRECT:
        scenarios = scenarios or ['success', 'install-failure', 'build-failure', 'missing-artifact', 'signed-artifact', 'ambiguous-artifact', 'dependency-chain', 'split-base', 'archive-identity', 'archive-race', 'invalid-signature', 'signature-required']
        if 'dry-run' in scenarios:
            raise SystemExit('UNVERIFIED: direct harness does not exercise CLI dry-run dispatch')
        with tempfile.TemporaryDirectory(prefix='archon-direct-harness-') as harness:
            BINARY = compile_direct_harness(harness)
            for scenario in scenarios: run_case(scenario)
    else:
        for scenario in scenarios or ['success', 'install-failure', 'build-failure', 'missing-artifact', 'dry-run']:
            run_case(scenario)
