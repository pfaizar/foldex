"""Reject stale builds and archive paths before compiling a distributable EXE."""
import hashlib
import json
from pathlib import Path
import struct
import sys
import tempfile
import unittest
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import package_installer as package


class InstallerPackaging(unittest.TestCase):
    def archive(self, root, extra=None, commit="commit", digest=None):
        binary = bytearray(128)
        binary[:2] = b"MZ"
        struct.pack_into("<I", binary, 0x3C, 64)
        binary[64:68] = b"PE\0\0"
        struct.pack_into("<H", binary, 68, 0x8664)
        archive = root / "portable.zip"
        prefix = "Foldex-0.1.0-windows-x64-portable"
        with zipfile.ZipFile(archive, "w") as zipped:
            zipped.writestr(f"{prefix}/Foldex.exe", binary)
            if extra:
                zipped.writestr(extra, b"untrusted")
        archive.with_name(f".{archive.stem}.json").write_text(json.dumps({
            "version": "0.1.0", "tag": "v0.1.0", "commit": commit,
            "archive_sha256": hashlib.sha256(archive.read_bytes()).hexdigest(),
            "executable_sha256": digest or hashlib.sha256(binary).hexdigest(),
        }), encoding="utf-8")
        return archive

    def test_extracts_matching_build(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            folder = package.extract_archive(self.archive(root), root / "stage", "0.1.0", "commit")
            self.assertTrue((folder / "Foldex.exe").is_file())
            self.assertFalse((folder / "BUILD-INFO.json").exists())

    def test_rejects_archive_traversal_and_windows_absolute_paths(self):
        for path in ["../outside", "Foldex-0.1.0-windows-x64-portable/../../outside", "C:/outside",
                     "Foldex-0.1.0-windows-x64-portable\\..\\outside", "/outside"]:
            with self.subTest(path=path), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                with self.assertRaises(ValueError):
                    package.extract_archive(self.archive(root, extra=path), root / "stage", "0.1.0", "commit")

    def test_rejects_archive_changed_after_validation(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            archive = self.archive(root)
            with zipfile.ZipFile(archive, "a") as zipped:
                zipped.writestr("Foldex-0.1.0-windows-x64-portable/extra.txt", "changed")
            with self.assertRaisesRegex(ValueError, "differs from CI"):
                package.extract_archive(archive, root / "stage", "0.1.0", "commit")

    def test_rejects_stale_commit_and_tampered_binary(self):
        for options in [{"commit": "stale"}, {"digest": "0" * 64}]:
            with self.subTest(options=options), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                with self.assertRaises(ValueError):
                    package.extract_archive(self.archive(root, **options), root / "stage", "0.1.0", "commit")


if __name__ == "__main__":
    unittest.main()
