#!/usr/bin/env python3
"""Gate contract tests; generated logs here are fixtures, not release evidence."""
import copy
import hashlib
import importlib.util
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("gate", Path(__file__).with_name("check-release-evidence.py"))
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)


class EvidenceTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.commit = "a" * 40
        log = self.root / "scenario.log"
        log.write_text("fixture scenario output\n")
        result = {"status": "PASS", "exit_code": 0, "log": log.name, "sha256": hashlib.sha256(log.read_bytes()).hexdigest()}
        self.document = {"schema_version": 1, "commit": self.commit, "environments": [{
            "distro": "fedora", "kind": "vm", "version": "fixture", "architecture": "x86_64", "kernel": "fixture", "run_id": "fixture",
            "boot_id_before": "11111111-1111-1111-1111-111111111111", "boot_id_after": "22222222-2222-2222-2222-222222222222",
            "scenarios": {name: copy.deepcopy(result) for name in gate.SCENARIOS}}]}

    def errors(self, document=None, distros=("fedora",)):
        return gate.validate(document or self.document, self.root, self.commit, distros)

    def test_complete_matched_evidence_passes(self):
        self.assertEqual(self.errors(), [])

    def test_fixture_kind_missing_distro_and_wrong_commit_block_release(self):
        self.document["environments"][0]["kind"] = "fixture"
        self.assertTrue(self.errors())
        self.assertTrue(self.errors(distros=("arch",)))
        self.document["commit"] = "b" * 40
        self.assertTrue(self.errors())

    def test_same_boot_missing_scenario_and_modified_log_block_release(self):
        env = self.document["environments"][0]
        env["boot_id_after"] = env["boot_id_before"]
        self.assertTrue(self.errors())
        del env["scenarios"]["signature_rejection"]
        self.assertTrue(self.errors())
        (self.root / "scenario.log").write_text("modified")
        self.assertTrue(self.errors())

    def test_paths_outside_evidence_directory_cannot_be_used_as_logs(self):
        self.document["environments"][0]["scenarios"]["upgrade"]["log"] = "/etc/passwd"
        self.assertTrue(self.errors())


if __name__ == "__main__":
    unittest.main()
