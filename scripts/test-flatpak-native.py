#!/usr/bin/env python3
"""Real signed Flatpak repository, isolated installation; no host package state."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

BRIDGE = Path(__file__).resolve().parents[1] / 'crates/archon-distro/src/flatpak_bridge.py'

def run(*args, **kwargs):
    result = subprocess.run(args, text=True, capture_output=True, **kwargs)
    if result.returncode:
        raise RuntimeError(f"{args}: {result.stderr} {result.stdout}")
    return result.stdout


def main():
    with tempfile.TemporaryDirectory(prefix='archon-flatpak-native-') as directory:
        root = Path(directory)
        keyring = root/'keys'; keyring.mkdir(mode=0o700)
        env = dict(os.environ, GNUPGHOME=str(keyring))
        run('gpg', '--batch', '--pinentry-mode', 'loopback', '--passphrase', '', '--quick-generate-key', 'Archon Disposable Test <test@invalid>', 'ed25519', 'sign', '1d', env=env)
        fingerprint = next(line.split(':')[9] for line in run('gpg','--batch','--with-colons','--list-keys',env=env).splitlines() if line.startswith('fpr:'))
        key = root/'key.gpg'; key.write_bytes(subprocess.check_output(['gpg','--batch','--export',fingerprint],env=env))
        repo = root/'repo'
        runtime=root/'runtime'; (runtime/'usr/bin').mkdir(parents=True)
        (runtime/'files').mkdir()
        (runtime/'metadata').write_text('[Runtime]\nname=org.nizho.ArchonTestPlatform\nruntime=org.nizho.ArchonTestPlatform/x86_64/1\nsdk=org.nizho.ArchonTestPlatform/x86_64/1\n')
        app=root/'app'; (app/'files/bin').mkdir(parents=True)
        (app/'files/bin/archon-test').write_text('#!/bin/sh\nexit 0\n'); (app/'files/bin/archon-test').chmod(0o755)
        (app/'metadata').write_text('[Application]\nname=org.nizho.ArchonTest\nruntime=org.nizho.ArchonTestPlatform/x86_64/1\nsdk=org.nizho.ArchonTestPlatform/x86_64/1\ncommand=archon-test\n')
        run('flatpak','build-export','--runtime',f'--gpg-sign={fingerprint}',str(repo),str(runtime),'1',env=env)
        run('flatpak','build-finish',str(app),env=env)
        run('flatpak','build-export',f'--gpg-sign={fingerprint}',str(repo),str(app),'stable',env=env)
        run('flatpak','build-update-repo',f'--gpg-sign={fingerprint}',str(repo),env=env)
        installation=root/'installation'
        # Register the remote in this isolated installation using libflatpak.
        import gi
        gi.require_version('Flatpak','1.0')
        from gi.repository import Flatpak,Gio,GLib
        native=Flatpak.Installation.new_for_path(Gio.File.new_for_path(str(installation)),True,None)
        remote=Flatpak.Remote.new('archon-test'); remote.set_url(repo.as_uri());remote.set_gpg_verify(True);remote.set_gpg_key(GLib.Bytes.new(key.read_bytes()));native.modify_remote(remote,None)
        def bridge(mode, **fields):
            output=run(sys.executable,str(BRIDGE),'--test-installation',str(installation),input=json.dumps(dict(mode=mode,**fields)))
            events=[json.loads(line) for line in output.splitlines()]
            return next(event['value'] for event in events if event['event']=='result')
        request=dict(action='install',package='app/org.nizho.ArchonTest/x86_64/stable',repository='archon-test')
        assert bridge('installed')==[]
        if os.environ.get('NIZHO_NATIVE_DBUS_TEST'):
            subprocess.run(['dbus-run-session','--',sys.executable,os.environ['NIZHO_NATIVE_DBUS_TEST'],os.environ['NIZHO_ARCHON_SERVICE'],str(installation),str(root/'service-state')],check=True)
        assert any(p['id']==request['package'] for p in bridge('search',query='ArchonTest',offset=0,limit=50))
        assert 'org.nizho.ArchonTestPlatform' in bridge('details',package=request['package'],repository='archon-test')['dependencies']
        plan=bridge('plan',request=request)
        assert bridge('installed')==[], 'planning mutated the installation'
        assert len(plan['changes'])>=2, 'runtime dependency was not planned'
        expired=subprocess.run([sys.executable,str(BRIDGE),'--test-installation',str(installation)],input=json.dumps(dict(mode='commit',request=request,expected=plan,expires_at=0)),text=True,capture_output=True)
        assert expired.returncode != 0 and bridge('installed')==[]
        cancelled=subprocess.run([sys.executable,str(BRIDGE),'--test-installation',str(installation)],input=json.dumps(dict(mode='commit',request=request,expected=plan))+'\ncancel\n',text=True,capture_output=True)
        assert cancelled.returncode != 0 and bridge('installed')==[]
        print('PASS expiry and native cancellation without installation')
        bridge('commit',request=request,expected=plan)
        assert any(p['name']=='org.nizho.ArchonTest' for p in bridge('installed'))
        print('PASS real signed installation and dependency resolution')
        (app/'files/bin/archon-test').write_text('#!/bin/sh\n# version two\nexit 0\n')
        run('flatpak','build-export',f'--gpg-sign={fingerprint}',str(repo),str(app),'stable',env=env)
        run('flatpak','build-update-repo',f'--gpg-sign={fingerprint}',str(repo),env=env)
        assert any(p['name']=='org.nizho.ArchonTest' for p in bridge('updates'))
        update=dict(request,action='update');update_plan=bridge('plan',request=update)
        bridge('commit',request=update,expected=update_plan)
        print('PASS real signed update discovery, planning and installation')
        # Replaying the old resolved state must fail, without accepting a new plan.
        replay=subprocess.run([sys.executable,str(BRIDGE),'--test-installation',str(installation)],input=json.dumps(dict(mode='commit',request=request,expected=plan)),text=True,capture_output=True)
        assert replay.returncode != 0, 'replay unexpectedly succeeded'
        print('PASS stale/replayed native plan rejection')
        request['action']='remove'; plan=bridge('plan',request=request); bridge('commit',request=request,expected=plan)
        assert bridge('installed')==[]
        print('PASS real removal')
        (app/'files/bin/archon-test').write_text('#!/bin/sh\n# unsigned version three\nexit 0\n')
        run('flatpak','build-export',str(repo),str(app),'stable',env=env)
        run('flatpak','build-update-repo',f'--gpg-sign={fingerprint}',str(repo),env=env)
        unsigned=dict(request,action='install')
        candidate=subprocess.run([sys.executable,str(BRIDGE),'--test-installation',str(installation)],input=json.dumps(dict(mode='plan',request=unsigned)),text=True,capture_output=True)
        if candidate.returncode==0:
            unsigned_plan=next(json.loads(line)['value'] for line in candidate.stdout.splitlines() if json.loads(line)['event']=='result')
            candidate=subprocess.run([sys.executable,str(BRIDGE),'--test-installation',str(installation)],input=json.dumps(dict(mode='commit',request=unsigned,expected=unsigned_plan)),text=True,capture_output=True)
        assert candidate.returncode != 0, 'unsigned artifact was accepted'
        assert bridge('installed')==[]
        print('PASS unsigned artifact rejection with signature policy still enabled')
        # A configured remote with verification disabled is never accepted.
        remote.set_gpg_verify(False);native.modify_remote(remote,None)
        rejected=subprocess.run([sys.executable,str(BRIDGE),'--test-installation',str(installation)],input=json.dumps(dict(mode='plan',request=request)),text=True,capture_output=True)
        assert rejected.returncode != 0
        print('PASS disabled-signature-policy rejection')
        print('Artifacts removed only from this test-owned temporary directory')

if __name__=='__main__':
    main()
