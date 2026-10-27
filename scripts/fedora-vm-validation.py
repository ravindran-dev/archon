#!/usr/bin/env python3
"""Real Fedora VM validation. Run only in a disposable VM; never on a host OS.

Stage one exits 75 once a real RPM commit is in progress. Reboot the disposable
VM, then run --resume to verify fail-closed recovery and write release evidence.
This harness is supplied but has not been executed in the agent environment.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time
import uuid

ROOT = Path("/var/lib/archon-vm-validation")
REPO_FILE = Path("/etc/yum.repos.d/archon-vm-validation.repo")
CACHE_CONFIG = Path("/etc/dnf/libdnf5.conf.d/99-archon-vm-validation.conf")
MARKER = Path("/run/archon-vm-commit-started")


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def run(args, log, expected=0, **kwargs):
    result = subprocess.run([str(arg) for arg in args], capture_output=True, text=True, **kwargs)
    with log.open("a") as stream:
        stream.write(json.dumps({"argv": [str(arg) for arg in args], "exit_code": result.returncode}) + "\n")
        stream.write(result.stdout + result.stderr + "\n")
    if expected is not None and result.returncode != expected:
        raise RuntimeError(f"Scenario command returned {result.returncode}: {args}; inspect {log}")
    return result


def events(result):
    return [json.loads(line) for line in result.stdout.splitlines() if line.strip()]


def plan(binary, operation, packages, log):
    result = run([binary, "transaction", "plan", operation, *packages, "--json"], log)
    record = next(event["transaction"] for event in events(result) if event["event"] == "plan")
    return record


def apply(binary, record, log, expected=0):
    return run([binary, "transaction", "apply", record["id"], "--yes", "--json"], log, expected=expected)


def build(name, version, key, log, signed=True, requires="", slow=False):
    top = ROOT / "build"
    for child in ["BUILD", "BUILDROOT", "RPMS", "SOURCES", "SPECS", "SRPMS"]:
        (top / child).mkdir(parents=True, exist_ok=True)
    spec = top / "SPECS" / f"{name}.spec"
    spec.write_text(f"""Name: {name}
Version: {version}
Release: 1
Summary: Disposable Archon VM validation fixture
License: MIT
BuildArch: noarch
AutoReqProv: no
{('Requires: ' + requires) if requires else ''}
%description
Disposable validation fixture.
%install
mkdir -p %{{buildroot}}/usr/share/{name}
echo {version} > %{{buildroot}}/usr/share/{name}/version
%files
/usr/share/{name}
""" + ("\n%post\ntouch /run/archon-vm-commit-started\nsleep 300\n" if slow else ""))
    run(["rpmbuild", "--define", f"_topdir {top}", "-bb", spec], log)
    rpm = top / "RPMS/noarch" / f"{name}-{version}-1.noarch.rpm"
    if signed:
        run(["rpmsign", "--define", "_openpgp_sign gpg", "--define", f"_openpgp_sign_id {key}",
             "--define", f"_gpg_name {key}", "--define", f"_gpg_path {ROOT / 'gnupg'}", "--addsign", rpm], log)
    shutil.copy2(rpm, ROOT / "repo" / rpm.name)
    run(["createrepo_c", "--update", ROOT / "repo"], log)


def success(log):
    return {"status": "PASS", "exit_code": 0, "log": log.name,
            "sha256": hashlib.sha256(log.read_bytes()).hexdigest()}


def check_guest(args):
    if not args.acknowledge_disposable_vm or os.geteuid() != 0:
        raise RuntimeError("This destructive VM harness requires root and --acknowledge-disposable-vm")
    if subprocess.run(["systemd-detect-virt", "--vm", "--quiet"]).returncode != 0:
        raise RuntimeError("A real disposable VM is required; host/container execution is refused")
    os_release = Path("/etc/os-release").read_text()
    if not re.search(r'^ID=["\']?fedora["\']?$', os_release, re.MULTILINE):
        raise RuntimeError("This harness validates Fedora only")
    for tool in ["gpg", "rpm", "rpmbuild", "rpmsign", "createrepo_c", "mount", "umount"]:
        if shutil.which(tool) is None:
            raise RuntimeError(f"Provision required guest tool: {tool}")


def start(args):
    if ROOT.exists():
        raise RuntimeError("Use a fresh VM snapshot; existing validation state must not be overwritten")
    ROOT.mkdir(mode=0o700)
    (ROOT / "repo").mkdir()
    keyring = ROOT / "gnupg"
    keyring.mkdir(mode=0o700)
    setup = ROOT / "setup.log"
    run(["gpg", "--homedir", keyring, "--batch", "--passphrase", "", "--quick-gen-key", "Archon VM Fixture <fixture@invalid>", "rsa2048", "sign", "1d"], setup)
    listing = run(["gpg", "--homedir", keyring, "--with-colons", "--list-secret-keys"], setup).stdout
    key = next(line.split(":")[9] for line in listing.splitlines() if line.startswith("fpr:"))
    public = run(["gpg", "--homedir", keyring, "--armor", "--export", key], setup).stdout
    (ROOT / "public.asc").write_text(public)
    run(["rpm", "--import", ROOT / "public.asc"], setup)
    REPO_FILE.write_text(f"[archon-vm-validation]\nname=Disposable validation\nbaseurl=file://{ROOT / 'repo'}\nenabled=1\ngpgcheck=1\nrepo_gpgcheck=0\nmetadata_expire=0\ngpgkey=file://{ROOT / 'public.asc'}\n")

    upgrade = ROOT / "upgrade.log"
    build("archon-vm-dependency", "1", key, upgrade)
    build("archon-vm-fixture", "1", key, upgrade, requires="archon-vm-dependency >= 1")
    first = plan(args.binary, "install", ["archon-vm-fixture"], upgrade)
    require(any(change["package"]["name"] == "archon-vm-dependency" for change in first["plan"]["changes"]), "Validation scenario failed; inspect the raw scenario log")
    apply(args.binary, first, upgrade)
    build("archon-vm-fixture", "2", key, upgrade, requires="archon-vm-dependency >= 1")
    second = plan(args.binary, "install", ["archon-vm-fixture"], upgrade)
    require(any(change["action"] == "upgrade" for change in second["plan"]["changes"]), "Validation scenario failed; inspect the raw scenario log")
    apply(args.binary, second, upgrade)
    require(run(["rpm", "-q", "--qf", "%{VERSION}", "archon-vm-fixture"], upgrade).stdout == "2", "Validation scenario failed; inspect the raw scenario log")

    failure = ROOT / "native_failure.log"
    result = run([args.binary, "transaction", "plan", "install", "archon-vm-missing-" + uuid.uuid4().hex, "--json"], failure, expected=None)
    require(result.returncode != 0, "Validation scenario failed; inspect the raw scenario log")
    require(any(event.get("error", {}).get("code") == "resolve_failed" for event in events(result)), "Validation scenario failed; inspect the raw scenario log")

    signature = ROOT / "signature_rejection.log"
    build("archon-vm-unsigned", "1", key, signature, signed=False)
    unsigned = plan(args.binary, "install", ["archon-vm-unsigned"], signature)
    result = apply(args.binary, unsigned, signature, expected=None)
    require(result.returncode != 0, "Validation scenario failed; inspect the raw scenario log")
    require(any(event.get("error", {}).get("code") == "signature_failed" for event in events(result)), "Validation scenario failed; inspect the raw scenario log")
    require(run(["rpm", "-q", "archon-vm-unsigned"], signature, expected=None).returncode != 0, "Validation scenario failed; inspect the raw scenario log")

    disk = ROOT / "disk_exhaustion.log"
    build("archon-vm-space", "1", key, disk)
    cache = ROOT / "small-cache"
    cache.mkdir()
    if CACHE_CONFIG.exists(): raise RuntimeError("Fixture cache config already exists")
    CACHE_CONFIG.parent.mkdir(parents=True, exist_ok=True)
    run(["mount", "-t", "tmpfs", "-o", "size=512m", "tmpfs", cache], disk)
    try:
        CACHE_CONFIG.write_text(f"[main]\ncachedir={cache}\n")
        low_space = plan(args.binary, "install", ["archon-vm-space"], disk)
        remaining = shutil.disk_usage(cache).free - 1024 * 1024
        with (cache / "filler").open("wb") as stream:
            chunk = b"\0" * (1024 * 1024)
            while remaining > 0:
                count = min(remaining, len(chunk))
                stream.write(chunk[:count]); remaining -= count
        result = apply(args.binary, low_space, disk, expected=None)
        require(result.returncode != 0, "Validation scenario failed; inspect the raw scenario log")
        require(any(event.get("error", {}).get("code") == "insufficient_disk" for event in events(result)), "Validation scenario failed; inspect the raw scenario log")
        require(run(["rpm", "-q", "archon-vm-space"], disk, expected=None).returncode != 0, "Validation scenario failed; inspect the raw scenario log")
    finally:
        CACHE_CONFIG.unlink(missing_ok=True)
        run(["umount", cache], disk)

    reboot = ROOT / "reboot_recovery.log"
    build("archon-vm-interrupt", "1", key, reboot, slow=True)
    interrupted = plan(args.binary, "install", ["archon-vm-interrupt"], reboot)
    pending = {"commit": args.commit, "id": interrupted["id"], "run_id": str(uuid.uuid4()),
               "boot_id_before": Path("/proc/sys/kernel/random/boot_id").read_text().strip()}
    (ROOT / "pending.json").write_text(json.dumps(pending))
    with reboot.open("a") as log:
        subprocess.Popen([args.binary, "transaction", "apply", interrupted["id"], "--yes", "--json"], stdout=log, stderr=log, start_new_session=True)
    deadline = time.monotonic() + 180
    while not MARKER.exists():
        if time.monotonic() > deadline: raise RuntimeError("Native commit did not reach fixture post-script")
        time.sleep(1)
    print("Real RPM commit is in progress. Reboot this disposable VM now, then rerun with --resume. No PASS evidence is produced until recovery succeeds.")
    return 75


def resume(args):
    pending = json.loads((ROOT / "pending.json").read_text())
    if pending["commit"] != args.commit: raise RuntimeError("Resume must use the same tested commit")
    after = Path("/proc/sys/kernel/random/boot_id").read_text().strip()
    if pending["boot_id_before"] == after: raise RuntimeError("The VM has not rebooted")
    log = ROOT / "reboot_recovery.log"
    result = run([args.binary, "transaction", "recover", pending["id"], "--json"], log)
    recovery = next(event for event in events(result) if event["event"] == "recovery")
    require(recovery["status"] == "uncertain" and not recovery["safe_to_retry"], "Validation scenario failed; inspect the raw scenario log")
    require(run([args.binary, "transaction", "apply", pending["id"], "--yes", "--json"], log, expected=None).returncode != 0, "Validation scenario failed; inspect the raw scenario log")
    run([args.binary, "check"], log, expected=None)
    run([args.binary, "history"], log, expected=None)
    release = {}
    for line in Path("/etc/os-release").read_text().splitlines():
        if "=" in line:
            key, value = line.split("=", 1); release[key] = value.strip('"')
    environment = {"distro": "fedora", "kind": "vm", "version": release["VERSION_ID"],
                   "architecture": os.uname().machine, "kernel": os.uname().release,
                   "run_id": pending["run_id"], "boot_id_before": pending["boot_id_before"], "boot_id_after": after,
                   "scenarios": {name: success(ROOT / (name + ".log")) for name in (
                       "upgrade", "native_failure", "signature_rejection", "disk_exhaustion", "reboot_recovery")}}
    (ROOT / "evidence.json").write_text(json.dumps({"schema_version": 1, "commit": args.commit, "environments": [environment]}, indent=2))
    print(f"Fedora VM evidence written to {ROOT / 'evidence.json'}. Preserve all logs and submit them for review; other distro families still need their own VM evidence.")
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True)
    parser.add_argument("--commit", required=True)
    parser.add_argument("--acknowledge-disposable-vm", action="store_true")
    parser.add_argument("--resume", action="store_true")
    args = parser.parse_args()
    if not re.fullmatch(r"[0-9a-f]{40}", args.commit): parser.error("--commit must be the exact tested commit SHA")
    args.binary = str(Path(args.binary).resolve(strict=True))
    check_guest(args)
    return resume(args) if args.resume else start(args)


if __name__ == "__main__":
    sys.exit(main())
