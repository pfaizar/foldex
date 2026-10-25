"""Guard the architecture and version checks that protect release downloads."""

import importlib.util
from pathlib import Path
import struct
import unittest


SPEC = importlib.util.spec_from_file_location("package_portable", Path(__file__).resolve().parents[1] / "package_portable.py")
PACKAGE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PACKAGE)


class ReleaseChecks(unittest.TestCase):
    def test_tag_must_match_the_package_version(self):
        PACKAGE.validate_tag("v0.1.0", "0.1.0")
        for tag in ["v0.2.0", "main", "v0.1.0/../other", "v0.1.0;command"]:
            with self.subTest(tag=tag), self.assertRaises(ValueError):
                PACKAGE.validate_tag(tag, "0.1.0")

    def test_x86_and_arm_executables_are_rejected(self):
        for machine in [0x14C, 0xAA64, 0x8664]:
            binary = bytearray(128)
            binary[:2] = b"MZ"
            struct.pack_into("<I", binary, 0x3C, 64)
            binary[64:68] = b"PE\0\0"
            struct.pack_into("<H", binary, 68, machine)
            if machine == 0x8664:
                PACKAGE.validate_x64(binary)
            else:
                with self.assertRaises(ValueError):
                    PACKAGE.validate_x64(binary)

    def test_invalid_or_truncated_executables_are_rejected(self):
        for binary in [b"", b"MZ", bytes(128), b"MZ" + bytes(62)]:
            with self.subTest(length=len(binary)), self.assertRaises(ValueError):
                PACKAGE.validate_x64(binary)


if __name__ == "__main__":
    unittest.main()
