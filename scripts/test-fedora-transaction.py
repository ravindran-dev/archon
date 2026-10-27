#!/usr/bin/env python3
"""Hermetic contract/fault tests. These are not real Fedora release evidence."""
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import selectors
import subprocess
import sys
from types import SimpleNamespace as NS
import unittest
from unittest.mock import patch

SOURCE = Path(__file__).resolve().parents[1] / "crates/archon-distro/src/fedora_transaction.py"
spec = importlib.util.spec_from_file_location("bridge", SOURCE)
bridge = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bridge)


class Transaction:
    def __init__(self, signatures=True, result=0):
        self.calls = []
        self.signatures = signatures
        self.result = result

    def download(self): self.calls.append("download")
    def check_gpg_signatures(self):
        self.calls.append("verify")
        return self.signatures
    def run(self):
        self.calls.append("run")
        return self.result
    def set_description(self, value): self.calls.append("description")
    def set_user_id(self, value): self.calls.append("user_id")


RESULTS = NS(**{"TransactionRunResult_" + name: index for index, name in enumerate([
    "SUCCESS", "ERROR_CHECK", "ERROR_GPG_CHECK", "ERROR_LOCK", "ERROR_RESOLVE", "ERROR_RERUN", "ERROR_SYSTEM_CHANGED", "ERROR_RPM_RUN"])})
LIB = NS(base=NS(Transaction=RESULTS))


class FedoraTransactionTests(unittest.TestCase):
    def setUp(self):
        self.plan = {"download_bytes": 1024, "install_bytes": 2048, "policy_fingerprint": "policy"}
        self.base = NS(get_config=lambda: NS(get_cachedir_option=lambda: NS(get_value=lambda: "/tmp")))

    def execute(self, transaction, checkpoint=lambda phase: None):
        with patch.object(bridge, "check_disk"), patch.object(bridge, "policy_fingerprint", return_value="policy"):
            bridge.execute(LIB, self.base, transaction, self.plan, checkpoint)

    def test_signatures_are_checked_before_native_commit(self):
        transaction = Transaction(signatures=False)
        with self.assertRaises(bridge.BridgeError) as error:
            self.execute(transaction)
        self.assertEqual(error.exception.code, "signature_failed")
        self.assertEqual(transaction.calls, ["download", "verify"])

    def test_disk_exhaustion_prevents_download_and_commit(self):
        transaction = Transaction()
        with patch.object(bridge.shutil, "disk_usage", return_value=NS(free=0)):
            with self.assertRaises(bridge.BridgeError) as error:
                bridge.execute(LIB, self.base, transaction, self.plan, lambda phase: None)
        self.assertEqual(error.exception.code, "insufficient_disk")
        self.assertEqual(transaction.calls, [])

    def test_checkpoint_persistence_failure_prevents_native_commit(self):
        transaction = Transaction()
        def checkpoint(phase):
            if phase == "committing": raise OSError("journal disk full")
        with self.assertRaises(OSError): self.execute(transaction, checkpoint)
        self.assertEqual(transaction.calls, ["download", "verify"])

    def test_system_changed_or_native_lock_failure_is_not_retried(self):
        for result in [RESULTS.TransactionRunResult_ERROR_LOCK, RESULTS.TransactionRunResult_ERROR_SYSTEM_CHANGED]:
            transaction = Transaction(result=result)
            with self.assertRaises(bridge.BridgeError) as error:
                self.execute(transaction)
            self.assertFalse(error.exception.uncertain)
            self.assertEqual(transaction.calls.count("run"), 1)

    def test_rpm_run_failure_requires_manual_recovery(self):
        with self.assertRaises(bridge.BridgeError) as error:
            self.execute(Transaction(result=RESULTS.TransactionRunResult_ERROR_RPM_RUN))
        self.assertTrue(error.exception.uncertain)

    def test_policy_change_after_download_blocks_commit(self):
        transaction = Transaction()
        with patch.object(bridge, "check_disk"), patch.object(bridge, "policy_fingerprint", return_value="changed"):
            with self.assertRaises(bridge.BridgeError) as error:
                bridge.execute(LIB, self.base, transaction, self.plan, lambda phase: None)
        self.assertEqual(error.exception.code, "stale_plan")
        self.assertNotIn("run", transaction.calls)

    def test_parent_eof_cannot_authorize_native_work(self):
        request = {"operation": "install", "packages": ["demo"]}
        message = {"schema_version": 1, "mode": "apply", "request": request}
        events = []
        transaction = Transaction()
        with patch.object(bridge.os, "geteuid", return_value=0), patch.object(bridge, "resolve", return_value=(self.base, transaction)), patch.object(bridge, "normalize_plan", return_value=self.plan):
            result = bridge.serve(io.StringIO(json.dumps(message) + '\n'), events.append, LIB)
        self.assertEqual(result, 1)
        self.assertEqual(events[-1]["code"], "protocol_error")
        self.assertEqual(transaction.calls, [])

    def test_request_options_paths_and_groups_are_rejected(self):
        for target in ["--nogpgcheck", "/tmp/a.rpm", "@world", "a\nb"]:
            with self.assertRaises(bridge.BridgeError):
                bridge.validate_request({"operation": "install", "packages": [target]})

    def test_native_stdio_cannot_consume_protocol_or_corrupt_json(self):
        wrapper = r'''
import os,sys
from pathlib import Path
from types import SimpleNamespace as NS
def configure():
    assert os.read(0,1) == b'', 'native stdin must be EOF'
    os.write(1,b'native output stays off protocol\n')
base=NS(load_config=configure,setup=lambda:None,get_repo_sack=lambda:NS(create_repos_from_system_configuration=lambda:None,load_repos=lambda:None))
transaction=NS(get_problems=lambda:0,get_transaction_groups=lambda:[],get_transaction_environments=lambda:[],get_transaction_packages=lambda:[])
class Goal:
    def __init__(self,base): pass
    def add_rpm_upgrade(self): pass
    def resolve(self): return transaction
class Query(list):
    def filter_installed(self): pass
sys.modules['libdnf5']=NS(base=NS(Base=lambda:base,Goal=Goal,GoalProblem_NO_PROBLEM=0),rpm=NS(PackageQuery=lambda base:Query()))
source=Path(sys.argv[1]).read_text()
source=source.replace('if __name__ == "__main__":', 'policy_fingerprint=lambda base: "c"*64\nif __name__ == "__main__":')
exec(compile(source,sys.argv[1],'exec'),{'__name__':'__main__'})
'''
        child = subprocess.Popen([sys.executable, "-I", "-c", wrapper, str(SOURCE)], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            child.stdin.write(json.dumps({"schema_version": 1, "mode": "plan", "request": {"operation": "upgrade", "packages": []}}) + "\n")
            child.stdin.flush()
            self.assertEqual(json.loads(child.stdout.readline())["event"], "ready")
            child.stdin.write('{"continue":true}\n'); child.stdin.flush()
            # Keep the decision pipe OPEN: native read(0) must not wait for a
            # future approval message or steal it from the protocol reader.
            with selectors.DefaultSelector() as selector:
                selector.register(child.stdout, selectors.EVENT_READ)
                self.assertTrue(selector.select(5), "native code blocked on the private protocol input")
            self.assertEqual(json.loads(child.stdout.readline())["event"], "plan")
            self.assertEqual(json.loads(child.stdout.readline())["event"], "complete")
            child.stdin.close(); child.stdin = None
            stdout, stderr = child.communicate(timeout=5)
            self.assertEqual(child.returncode, 0)
            self.assertEqual(stdout, "")
            self.assertIn("native output stays off protocol", stderr)
        finally:
            if child.poll() is None: child.kill()
            child.communicate()

    def test_native_settings_and_plugins_are_not_disabled(self):
        calls = []
        class Goal:
            def __init__(self, base): pass
            def add_rpm_install(self, value): calls.append(("install", value))
            def resolve(self): return NS(get_problems=lambda: 0)
        sack = NS(create_repos_from_system_configuration=lambda: calls.append("repos"), load_repos=lambda: calls.append("load"))
        base = NS(load_config=lambda: calls.append("config"), setup=lambda: calls.append("setup"), get_repo_sack=lambda: sack)
        lib = NS(base=NS(Base=lambda: base, Goal=Goal, GoalProblem_NO_PROBLEM=0))
        bridge.resolve(lib, {"operation": "install", "packages": ["demo"]})
        self.assertEqual(calls, ["config", "setup", "repos", "load", ("install", "demo")])

    def test_policy_fingerprint_changes_when_repo_configuration_changes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            repo = root / "fixture.repo"
            repo.write_text("gpgcheck=1\n")
            first = bridge.fingerprint_files([root])
            repo.write_text("gpgcheck=0\n")
            self.assertNotEqual(first, bridge.fingerprint_files([root]))
            repo.unlink()
            self.assertNotEqual(first, bridge.fingerprint_files([root]))

    def test_native_lock_contention_fails_before_loading_or_resolving(self):
        calls = []
        sack = NS(create_repos_from_system_configuration=lambda: None, load_repos=lambda: calls.append("load"))
        def lock(access, blocking):
            calls.append((access, blocking))
            return False
        base = NS(load_config=lambda: None, setup=lambda: None, get_repo_sack=lambda: sack, lock_system_repo=lock)
        lib = NS(base=NS(Base=lambda: base), utils=NS(LockAccess_WRITE="write", LockAccess_READ="read", LockBlocking_NON_BLOCKING="nonblocking"))
        with self.assertRaises(bridge.BridgeError) as error:
            bridge.resolve(lib, {"operation": "upgrade", "packages": []}, write=True)
        self.assertEqual(error.exception.code, "native_locked")
        self.assertEqual(calls, [("write", "nonblocking")])

    def test_plan_preserves_dependency_reason_epoch_replacements_and_sizes(self):
        def package(name, version, installed):
            return NS(get_name=lambda: name, get_evr=lambda: version,
                      get_arch=lambda: "x86_64", get_nevra=lambda: name + "-" + version + ".x86_64",
                      get_reason=lambda: 1, get_rpmdbid=lambda: 5,
                      get_repo_id=lambda: "@System" if installed else "fixture",
                      get_download_size=lambda: 1024, get_install_size=lambda: 2048,
                      get_checksum=lambda: NS(get_type_str=lambda: "sha256", get_checksum=lambda: "a" * 64))
        old, new = package("old-demo", "1.0-1", True), package("demo", "2:2.0-1", False)
        changes = [NS(get_action=lambda: "Install", get_package=lambda: new, get_reason=lambda: "Dependency", get_replaces=lambda: [old], get_replaced_by=lambda: []),
                   NS(get_action=lambda: "Replaced", get_package=lambda: old, get_reason=lambda: "User", get_replaces=lambda: [], get_replaced_by=lambda: [new])]
        class Query(list):
            def filter_installed(self): pass
        lib = NS(rpm=NS(PackageQuery=lambda base: Query([old])), transaction=NS(
            transaction_item_action_to_string=lambda action: action,
            transaction_item_reason_to_string=lambda reason: reason,
            transaction_item_action_is_inbound=lambda action: action == "Install"))
        transaction = NS(get_transaction_groups=lambda: [], get_transaction_environments=lambda: [], get_transaction_packages=lambda: changes)
        with patch.object(bridge, "policy_fingerprint", return_value="policy"):
            plan = bridge.normalize_plan(lib, self.base, transaction, {"operation": "install", "packages": ["demo"]})
        self.assertEqual(plan["download_bytes"], 1024)
        self.assertEqual(plan["install_bytes"], 2048)
        self.assertEqual(plan["changes"][0]["reason"], "Dependency")
        self.assertEqual(plan["changes"][0]["package"]["version"], "2:2.0-1")
        self.assertEqual(plan["changes"][1]["download_bytes"], 0)
        self.assertEqual(plan["changes"][1]["replaced_by"][0]["name"], "demo")


if __name__ == "__main__":
    unittest.main()
