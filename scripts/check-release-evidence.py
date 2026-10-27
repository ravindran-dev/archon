#!/usr/bin/env python3
"""Fail-closed release evidence gate. Fixture tests are never VM certification."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys

DISTROS = ("arch", "debian", "fedora", "suse", "alpine", "nixos", "void", "gentoo")
SCENARIOS = ("upgrade", "native_failure", "signature_rejection", "disk_exhaustion", "reboot_recovery")


def validate(document, directory, commit, distros):
    errors = []
    if document.get("schema_version") != 1:
        errors.append("unsupported evidence schema")
    if document.get("commit") != commit or not re.fullmatch(r"[0-9a-f]{40}", commit):
        errors.append("evidence must match the exact release commit")
    environments = document.get("environments", [])
    if not isinstance(environments, list):
        return errors + ["environments must be a list"]
    directory = directory.resolve()
    for distro in distros:
        candidates = [env for env in environments if isinstance(env, dict) and env.get("distro") == distro]
        # Do not combine passing scenarios from unrelated test machines/runs.
        successful = False
        reasons = []
        for env in candidates:
            problems = []
            if env.get("kind") != "vm" or not all(isinstance(env.get(field), str) and env[field].strip() for field in ("version", "architecture", "kernel", "run_id")):
                problems.append("real VM identity is required")
            boot_before, boot_after = env.get("boot_id_before"), env.get("boot_id_after")
            if not all(isinstance(value, str) and re.fullmatch(r"[0-9a-f-]{36}", value) for value in (boot_before, boot_after)) or boot_before == boot_after:
                problems.append("reboot recovery requires distinct recorded boot IDs")
            scenarios = env.get("scenarios", {})
            if not isinstance(scenarios, dict):
                problems.append("scenarios must be a mapping")
                scenarios = {}
            for name in SCENARIOS:
                result = scenarios.get(name, {})
                if not isinstance(result, dict) or result.get("status") != "PASS" or type(result.get("exit_code")) is not int or result["exit_code"] != 0:
                    problems.append(name + ": successful verification scenario is missing")
                    continue
                log_name = result.get("log")
                if not isinstance(log_name, str):
                    problems.append(name + ": missing raw evidence")
                    continue
                log = (directory / log_name).resolve()
                if not log.is_relative_to(directory) or not log.is_file() or not 0 < log.stat().st_size <= 50 * 1024 * 1024:
                    problems.append(name + ": raw evidence must be a bounded file within the evidence directory")
                    continue
                if hashlib.sha256(log.read_bytes()).hexdigest() != result.get("sha256"):
                    problems.append(name + ": raw evidence checksum mismatch")
            if not problems:
                successful = True
                break
            reasons.extend(problems)
        if not successful:
            errors.append(distro + ": " + ("; ".join(dict.fromkeys(reasons)) if candidates else "VM evidence is missing"))
    return errors


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("evidence", type=Path)
    parser.add_argument("--distro", choices=DISTROS, action="append")
    args = parser.parse_args()
    commit = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
    dirty = subprocess.check_output(["git", "status", "--porcelain"], text=True)
    errors = ["release working tree must be clean"] if dirty.strip() else []
    try:
        document = json.loads(args.evidence.read_text())
        if not isinstance(document, dict): raise ValueError("evidence must be an object")
        errors.extend(validate(document, args.evidence.parent, commit, args.distro or DISTROS))
    except (OSError, ValueError, TypeError) as error:
        errors.append(str(error))
    print(json.dumps({"schema_version": 1, "release_ready": not errors, "commit": commit, "errors": errors}, indent=2))
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
