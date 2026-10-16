import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import tomllib
from types import SimpleNamespace
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parent.parent
SPEC = importlib.util.spec_from_file_location("stage_native", ROOT / "scripts/stage-native.py")
STAGE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(STAGE)
VERSION = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]["version"]


class CandidateStaging(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.binary = self.root / "compiled binary"
        self.binary.write_bytes(b"test executable bytes")
        self.output = self.root / "candidate with spaces é"
        self.process = patch.object(
            STAGE.subprocess, "run",
            return_value=SimpleNamespace(stdout=f"last-pane-guard {VERSION}\n"),
        )
        self.mock_run = self.process.start()
        self.addCleanup(self.process.stop)
        self.platform = patch.object(STAGE.platform, "system", return_value="Darwin")
        self.platform.start()
        self.addCleanup(self.platform.stop)

    def test_manifest_checksums_permissions_and_repeatability(self):
        self.assertIn("staged", STAGE.stage(self.binary, self.output))
        manifest = tomllib.loads((self.output / "herdr-plugin.toml").read_text())
        self.assertEqual(manifest["version"], VERSION)
        self.assertEqual(manifest["platforms"], ["macos"])
        for section in ("startup", "actions", "events", "panes"):
            self.assertTrue(all(item["command"][0] == "bin/last-pane-guard" for item in manifest[section]))
        self.assertEqual(len(manifest["events"]), 16)
        metadata = json.loads((self.output / "checksums.json").read_text())
        for name, expected in metadata["sha256"].items():
            self.assertEqual(hashlib.sha256((self.output / name).read_bytes()).hexdigest(), expected)
        self.assertEqual((self.output / "bin/last-pane-guard").stat().st_mode & 0o777, 0o755)
        self.assertEqual(STAGE.stage(self.binary, self.output), "candidate unchanged")
        self.assertEqual(self.mock_run.call_args.args[0][0], str(self.binary.resolve()))

    def test_changed_destination_is_not_overwritten(self):
        STAGE.stage(self.binary, self.output)
        sentinel = self.output / "LICENSE"
        sentinel.write_bytes(b"keep this change")
        with self.assertRaises(ValueError):
            STAGE.stage(self.binary, self.output)
        self.assertEqual(sentinel.read_bytes(), b"keep this change")

    def test_version_mismatch_creates_nothing(self):
        self.mock_run.return_value.stdout = "last-pane-guard wrong-version\n"
        with self.assertRaises(ValueError):
            STAGE.stage(self.binary, self.output)
        self.assertFalse(self.output.exists())

    def test_unsupported_platform_creates_nothing(self):
        with patch.object(STAGE.platform, "system", return_value="Windows"):
            with self.assertRaises(ValueError):
                STAGE.stage(self.binary, self.output)
        self.assertFalse(self.output.exists())

    def test_symlink_destination_is_rejected(self):
        destination = self.root / "other"
        destination.mkdir()
        self.output.symlink_to(destination, target_is_directory=True)
        with self.assertRaises(ValueError):
            STAGE.stage(self.binary, self.output)
        self.assertEqual(list(destination.iterdir()), [])


if __name__ == "__main__":
    unittest.main()
