"""Pinned official Windows runtimes; no host installations or third-party dependencies."""
from __future__ import annotations

import hashlib
import os
import shutil
import stat
import struct
import subprocess
import tempfile
import urllib.request
import zipfile
from pathlib import Path

from package import validate_pe_image

NODE_VERSION = "22.14.0"
PYTHON_VERSION = "3.13.2"
# Node: official SHASUMS256.txt; Python: SHA-256 of PSF's NuGet artifacts.
SOURCES = {
    "x64": (0x8664, "python",
            "55b639295920b219bb2acbcfa00f90393a2789095b7323f79475c9f34795f217",
            "a8f8f67d36c098787e845079c53f0e760d2841186d1a3f87c2ec6468527c1017"),
    "arm64": (0xAA64, "pythonarm64",
              "2d71f5f9b2fffa33baa108c07d74b0d24e0c3dd8f441d567772ae0e3dd4b1a22",
              "515d39e9eff8af5419b822d50696f5ad74d826a5f5d7984f48750483a56daff5"),
}


def pe_arch(data: bytes) -> str:
    if len(data) < 64 or data[:2] != b"MZ":
        raise ValueError("invalid PE image")
    offset = struct.unpack_from("<I", data, 60)[0]
    if offset + 24 > len(data) or data[offset:offset + 4] != b"PE\0\0":
        raise ValueError("invalid PE header")
    machine = struct.unpack_from("<H", data, offset + 4)[0]
    arch = next((arch for arch, source in SOURCES.items() if source[0] == machine), None)
    if arch is None:
        raise ValueError(f"unsupported Windows architecture: PE machine 0x{machine:04x}")
    validate_pe_image(data, expected=machine)
    return arch


def download(url: str, destination: Path, digest: str) -> None:
    try:
        sha = hashlib.sha256()
        with urllib.request.urlopen(url, timeout=60) as response, destination.open("wb") as out:
            if not response.geturl().startswith("https://"):
                raise ValueError("refusing non-HTTPS redirect")
            size = 0
            while chunk := response.read(1024 * 1024):
                size += len(chunk)
                if size > 256 * 1024 * 1024:
                    raise ValueError("download exceeds 256 MiB limit")
                sha.update(chunk)
                out.write(chunk)
        if sha.hexdigest() != digest:
            raise ValueError("SHA-256 mismatch (incomplete or changed artifact)")
    except Exception as exc:
        destination.unlink(missing_ok=True)
        raise RuntimeError(f"runtime download failed: {url}: {exc}") from exc


def extract_runtime(archive: Path, prefix: str, destination: Path) -> None:
    """Extract only a runtime subtree into a fresh directory; reject Windows aliases."""
    destination.mkdir(parents=True, exist_ok=False)
    seen = set()
    total = 0
    with zipfile.ZipFile(archive) as zf:
        for member in zf.infolist():
            name = member.filename
            parts = name.rstrip("/").split("/")
            reserved = {"con", "prn", "aux", "nul", *(f"com{i}" for i in range(1, 10)),
                        *(f"lpt{i}" for i in range(1, 10))}
            if (not name or "\\" in name or any(
                not p or p in (".", "..") or p.endswith((".", " "))
                or any(c in p for c in ':<>"|?*') or any(ord(c) < 32 for c in p)
                or p.split(".")[0].lower() in reserved for p in parts
            )):
                raise ValueError(f"unsafe ZIP path: {name!r}")
            mode = member.external_attr >> 16
            if stat.S_IFMT(mode) not in (0, stat.S_IFREG, stat.S_IFDIR):
                raise ValueError(f"unsupported ZIP file type: {name}")
            key = name.rstrip("/").casefold()
            if key in seen:
                raise ValueError(f"duplicate ZIP path: {name}")
            seen.add(key)
            total += member.file_size
            if total > 1024 * 1024 * 1024:
                raise ValueError("ZIP exceeds 1 GiB expanded limit")
            if not name.startswith(prefix) or member.is_dir():
                continue
            relative = name[len(prefix):]
            if not relative:
                continue
            path = destination / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            with zf.open(member) as source, path.open("xb") as out:
                shutil.copyfileobj(source, out)  # ZipFile checks CRC while reading.


def validate_runtime(root: Path, kind: str, arch: str) -> None:
    required = ("node.exe", "npm.cmd", "npx.cmd", "node_modules/npm/bin/npm-cli.js",
                "node_modules/npm/bin/npx-cli.js") if kind == "node" else (
                    "python.exe", "python3.dll", "python313.dll", "Lib/os.py",
                    "Lib/encodings/__init__.py", "Lib/ensurepip/__init__.py",
                    "Lib/ensurepip/__main__.py")
    for name in required:
        path = root / name
        if not path.is_file() or not path.stat().st_size:
            raise ValueError(f"incomplete {kind} runtime: missing {name}")
    if kind == "python" and not list((root / "Lib/ensurepip/_bundled").glob("pip-*.whl")):
        raise ValueError("Python runtime has no bundled pip wheel")
    for path in root.rglob("*"):
        if path.suffix.lower() in (".exe", ".dll", ".pyd"):
            if pe_arch(path.read_bytes()) != arch:
                raise ValueError(f"{kind} architecture mismatch ({arch} required): {path.name}")


def prepare_python(root: Path) -> None:
    # Ignore build-machine Python configuration and install only bundled wheels.
    env = {key: value for key, value in os.environ.items()
           if not key.upper().startswith(("PYTHON", "PIP_"))}
    python = str((root / "python.exe").resolve())
    for args in (("-I", "-m", "ensurepip", "--upgrade", "--default-pip"),
                 ("-I", "-m", "pip", "--version")):
        result = subprocess.run([python, *args], env=env, cwd=root,
                                capture_output=True, text=True, errors="replace", timeout=120)
        if result.returncode:
            raise RuntimeError(f"bundled Python {' '.join(args)} failed: "
                               f"{result.stdout}\n{result.stderr}")
    if not (root / "Scripts/pip.exe").is_file():
        raise ValueError("ensurepip did not create Scripts/pip.exe")
    shutil.copyfile(root / "python.exe", root / "python3.exe")
    # ensurepip's .exe launchers embed the staging path. Relative .cmd wrappers move safely.
    for name in ("pip", "pip3", "pip3.13"):
        (root / "Scripts" / f"{name}.exe").unlink(missing_ok=True)
        (root / "Scripts" / f"{name}.cmd").write_text(
            '@echo off\n"%~dp0..\\python.exe" -m pip %*\n', encoding="utf-8")


def smoke_test(root: Path) -> None:
    if os.name != "nt":
        raise RuntimeError("Windows full runtime verification requires a Windows build host")
    env = {key: value for key, value in os.environ.items()
           if not key.upper().startswith(("PYTHON", "PIP_")) and key.upper() != "PATH"}
    node = root / "runtimes/node"
    python = root / "runtimes/python"
    env["PATH"] = os.pathsep.join((str(node), str(python), str(python / "Scripts"),
                                  os.environ.get("PATH", "")))
    commands = ([str(node / "node.exe"), "--version"],
                ["cmd.exe", "/d", "/c", "npm.cmd --version"],
                [str(python / "python.exe"), "-I", "-m", "pip", "--version"],
                [str(python / "python3.exe"), "-I", "--version"],
                ["cmd.exe", "/d", "/c", "pip.cmd --version"])
    for command in commands:
        result = subprocess.run(command, cwd=root, env=env, capture_output=True,
                                text=True, errors="replace", timeout=120)
        if result.returncode:
            raise RuntimeError(f"runtime smoke test failed: {command}: "
                               f"{result.stdout}\n{result.stderr}")
        print(result.stdout.strip())


def build_full_zip(exe: Path, output: Path) -> None:
    arch = pe_arch(exe.read_bytes())
    _, python_id, node_hash, python_hash = SOURCES[arch]
    node_name = f"node-v{NODE_VERSION}-win-{arch}"
    artifacts = (
        ("node", f"https://nodejs.org/dist/v{NODE_VERSION}/{node_name}.zip", node_hash, node_name + "/"),
        ("python", f"https://api.nuget.org/v3-flatcontainer/{python_id}/{PYTHON_VERSION}/"
         f"{python_id}.{PYTHON_VERSION}.nupkg", python_hash, "tools/"),
    )
    # Stage beside the destination: failure never publishes a partial full ZIP.
    with tempfile.TemporaryDirectory(prefix=".nova-full-", dir=output.parent) as temp:
        work = Path(temp)
        for kind, url, digest, prefix in artifacts:
            print(f"Downloading {kind} ({arch}): {url}")
            archive = work / f"{kind}.zip"
            download(url, archive, digest)
            runtime = work / "runtimes" / kind
            extract_runtime(archive, prefix, runtime)
            validate_runtime(runtime, kind, arch)
            if kind == "python":
                prepare_python(runtime)
        # Move before execution to catch launchers tied to the ensurepip staging path.
        relocated = work / "portable check"
        relocated.mkdir()
        (work / "runtimes").rename(relocated / "runtimes")
        smoke_test(relocated.resolve())
        staged = work / "full.zip"
        with zipfile.ZipFile(staged, "w", zipfile.ZIP_DEFLATED) as zf:
            zf.write(exe, exe.name)
            for path in sorted((relocated / "runtimes").rglob("*")):
                if path.is_file():
                    zf.write(path, path.relative_to(relocated).as_posix())
        with zipfile.ZipFile(staged) as zf:
            if zf.testzip() is not None:
                raise ValueError("full ZIP failed CRC verification")
        staged.replace(output)
