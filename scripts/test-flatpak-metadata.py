#!/usr/bin/env python3
"""Untrusted AppStream parser tests; no package database or network operations."""
import gzip
import importlib.util
from pathlib import Path
import tempfile
import unittest

source = Path(__file__).resolve().parents[1] / "crates/archon-distro/src/flatpak_bridge.py"
spec = importlib.util.spec_from_file_location("flatpak_bridge", source)
bridge = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bridge)


class MetadataTests(unittest.TestCase):
    def parse(self, content, details=None):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "appstream.xml.gz"
            with gzip.open(path, "wb") as stream:
                stream.write(content)
            return bridge.read_appstream(path, details)

    def test_search_keeps_summaries_and_details_select_one_component(self):
        xml = b'<components><component><id>org.test.App.desktop</id><name>Test</name><summary>Summary</summary><description><p>Description</p></description><categories><category>Utility</category></categories></component><component><id>org.test.Other</id></component></components>'
        result = self.parse(xml)
        self.assertEqual(result["org.test.App"]["categories"], ["Utility"])
        self.assertNotIn("description", result["org.test.App"])
        details = self.parse(xml, "org.test.App")
        self.assertEqual(list(details), ["org.test.App"])
        self.assertEqual(details["org.test.App"]["description"], "Description")

    def test_rejects_dtd_entities_and_malformed_roots(self):
        for xml in (b'<!DOCTYPE components [<!ENTITY x SYSTEM "file:///etc/passwd">]><components/>',
                    b'<other/>', b'<components><component>'):
            with self.assertRaises((ValueError, __import__('xml.etree.ElementTree', fromlist=['ParseError']).ParseError)):
                self.parse(xml)

    def test_decompressed_metadata_bound(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "large.xml.gz"
            with gzip.open(path, "wb") as stream:
                stream.write(b'<components><!--')
                for _ in range(65):
                    stream.write(b'x' * 1048576)
                stream.write(b'--></components>')
            with self.assertRaisesRegex(ValueError, "exceeds bound"):
                bridge.read_appstream(path)


if __name__ == "__main__":
    unittest.main()
