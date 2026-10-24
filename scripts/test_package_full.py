"""Offline checks: python3 -m unittest discover -s scripts -p 'test_package_full.py'."""
import hashlib
import io
import struct
import tempfile
import unittest
import zipfile
from pathlib import Path
from unittest.mock import patch

import package_full as full


def pe(arch="x64"):
    data = bytearray(256)
    data[:2] = b"MZ"
    struct.pack_into("<I", data, 60, 64)
    data[64:68] = b"PE\0\0"
    struct.pack_into("<HH", data, 68, full.SOURCES[arch][0], 1)
    struct.pack_into("<H", data, 84, 2)
    struct.pack_into("<H", data, 88, 0x20B)
    return bytes(data)


class FullPackageTests(unittest.TestCase):
    def test_architecture_and_truncation(self):
        for arch in full.SOURCES:
            self.assertEqual(full.pe_arch(pe(arch)), arch)
        for data in (b"MZ", pe()[:91], pe()[:68] + b"\x4c\x01" + pe()[70:]):
            with self.assertRaises(ValueError):
                full.pe_arch(data)

    def test_safe_extraction(self):
        for name in ("tools/../escape", "/escape", "tools/C:evil", "tools/a\\b",
                     "tools/NUL.txt", "tools/a.", "tools//a", "tools/./a"):
            with self.subTest(name=name), tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                archive = root / "bad.zip"
                with zipfile.ZipFile(archive, "w") as zf:
                    zf.writestr(name, b"bad")
                with self.assertRaises(ValueError):
                    full.extract_runtime(archive, "tools/", root / "out")
                self.assertFalse((root / "escape").exists())
        for entries in (("tools/a", "tools/A"), ("tools/link",)):
            with tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                with zipfile.ZipFile(root / "bad.zip", "w") as zf:
                    for name in entries:
                        member = zipfile.ZipInfo(name)
                        if name.endswith("link"):
                            member.external_attr = 0o120777 << 16
                        zf.writestr(member, "target")
                with self.assertRaises(ValueError):
                    full.extract_runtime(root / "bad.zip", "tools/", root / "out")

    def test_download_hash_and_failure(self):
        class Response(io.BytesIO):
            def geturl(self):
                return "https://example.test/runtime.zip"
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "runtime.zip"
            with patch.object(full.urllib.request, "urlopen", return_value=Response(b"zip")):
                full.download("https://example.test", path, hashlib.sha256(b"zip").hexdigest())
            self.assertEqual(path.read_bytes(), b"zip")
            with patch.object(full.urllib.request, "urlopen", return_value=Response(b"bad")):
                with self.assertRaisesRegex(RuntimeError, "SHA-256 mismatch"):
                    full.download("https://example.test", path, "0" * 64)
            self.assertFalse(path.exists())
            with patch.object(full.urllib.request, "urlopen", side_effect=OSError("offline")):
                with self.assertRaisesRegex(RuntimeError, "runtime download failed.*offline"):
                    full.download("https://example.test", path, "0" * 64)

    def test_python_preparation(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / "python.exe").write_bytes(pe())
            with patch.object(full.subprocess, "run") as run:
                run.return_value.returncode = 0
                full.prepare_python(root)
            self.assertEqual((root / "python3.exe").read_bytes(), pe())
            self.assertEqual(run.call_args_list[0].args[0][1:], ["-I", "-m", "pip", "--version"])
            self.assertFalse(list((root / "Scripts").glob("*.exe")))
            self.assertIn('%~dp0..\\python.exe', (root / "Scripts/pip.cmd").read_text())
            with patch.object(full.subprocess, "run") as run:
                run.return_value.returncode = 1
                with self.assertRaisesRegex(RuntimeError, "pip --version failed"):
                    full.prepare_python(root)

    def test_smoke_commands_and_failure(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            with patch.object(full.os, "name", "nt"), patch.object(full.subprocess, "run") as run:
                run.return_value.returncode = 0
                run.return_value.stdout = "version"
                full.smoke_test(root)
                commands = [call.args[0] for call in run.call_args_list]
                self.assertEqual(len(commands), 5)
                self.assertEqual(commands[0], [str(root / "runtimes/node/node.exe"), "--version"])
                self.assertIn("npm.cmd --version", commands[1])
                self.assertIn("pip", commands[2])
                self.assertTrue(commands[3][0].endswith("python3.exe"))
                self.assertIn("pip.cmd --version", commands[4])
                run.return_value.returncode = 1
                with self.assertRaisesRegex(RuntimeError, "smoke test failed"):
                    full.smoke_test(root)

    def test_full_layout_validation_and_atomic_failure(self):
        for arch in full.SOURCES:
            with self.subTest(arch=arch), tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                exe = root / "Nova.exe"
                exe.write_bytes(pe(arch))
                output = root / "nova-full-test.zip"

                def fake_download(url, destination, digest):
                    node = destination.stem == "node"
                    prefix = f"node-v{full.NODE_VERSION}-win-{arch}/" if node else "tools/"
                    files = {"node.exe": pe(arch), "npm.cmd": b"npm", "npx.cmd": b"npx",
                             "node_modules/npm/bin/npm-cli.js": b"npm",
                             "node_modules/npm/bin/npx-cli.js": b"npx"} if node else {
                                 "python.exe": pe(arch), "python3.dll": pe(arch),
                                 "python313.dll": pe(arch), "Lib/os.py": b"os",
                                 "Lib/encodings/__init__.py": b"encoding",
                                 "Lib/ensurepip/__init__.py": b"ensurepip",
                                 "Lib/ensurepip/__main__.py": b"main",
                                 "Lib/ensurepip/_bundled/pip-test.whl": b"wheel"}
                    self.assertIn(arch if node else full.SOURCES[arch][1], url)
                    with zipfile.ZipFile(destination, "w") as zf:
                        for name, data in files.items():
                            zf.writestr(prefix + name, data)
                        zf.writestr("metadata.txt", "not a runtime file")

                with patch.object(full, "download", side_effect=fake_download), \
                     patch.object(full, "prepare_python") as prepare, \
                     patch.object(full, "smoke_test") as smoke:
                    full.build_full_zip(exe, output)
                prepare.assert_called_once()
                smoke.assert_called_once()
                with zipfile.ZipFile(output) as zf:
                    self.assertIsNone(zf.testzip())
                    self.assertEqual(zf.read("Nova.exe"), exe.read_bytes())
                    self.assertIn("runtimes/node/npm.cmd", zf.namelist())
                    self.assertIn("runtimes/python/Lib/ensurepip/__main__.py", zf.namelist())
                    self.assertTrue(all(n == "Nova.exe" or n.startswith("runtimes/") for n in zf.namelist()))
                    zf.extractall(root / "check")
                runtime = root / "check/runtimes/node"
                (runtime / "node.exe").write_bytes(pe("arm64" if arch == "x64" else "x64"))
                with self.assertRaisesRegex(ValueError, "architecture mismatch"):
                    full.validate_runtime(runtime, "node", arch)
                (runtime / "npm.cmd").unlink()
                with self.assertRaisesRegex(ValueError, "incomplete"):
                    full.validate_runtime(runtime, "node", arch)
                previous = output.read_bytes()
                with patch.object(full, "download", side_effect=RuntimeError("offline")):
                    with self.assertRaises(RuntimeError):
                        full.build_full_zip(exe, output)
                self.assertEqual(output.read_bytes(), previous)
                self.assertFalse(list(root.glob(".nova-full-*")))


if __name__ == "__main__":
    unittest.main()
