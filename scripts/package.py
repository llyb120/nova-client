#!/usr/bin/env python3
# 打包 Nova 更新 zip（由 GitHub Release 分发，不上传私服）
# Windows：Nova.exe → nova-{ver}.zip
# macOS：Nova → nova-macos-{arch}-{ver}.zip
import argparse
import json
import os
import platform
import struct
import subprocess
import sys
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CONF = ROOT / "src-tauri" / "tauri.conf.json"
PKG = ROOT / "package.json"


def is_windows() -> bool:
    return os.name == "nt" or sys.platform.startswith("win")


def is_macos() -> bool:
    return sys.platform == "darwin"


def update_channel() -> str:
    if is_macos():
        machine = platform.machine().lower()
        arch = "aarch64" if machine in ("arm64", "aarch64") else "x86_64"
        return f"nova-macos-{arch}"
    return "nova"


def binary_name() -> str:
    return "Nova.exe" if is_windows() else "Nova"


def expected_pe_machine() -> int | None:
    if not is_windows():
        return None
    machine = platform.machine().lower()
    if machine in ("amd64", "x86_64"):
        return 0x8664
    if machine in ("x86", "i386", "i686"):
        return 0x014C
    if machine in ("arm64", "aarch64"):
        return 0xAA64
    return None


def pe_machine_name(machine: int) -> str:
    return {0x014C: "x86", 0x8664: "x64", 0xAA64: "arm64"}.get(machine, "unknown")


def validate_pe_image(data: bytes) -> None:
    if len(data) < 0x40:
        raise ValueError("文件太小")
    if data[:2] != b"MZ":
        raise ValueError("缺少 MZ 文件头")
    pe_offset = struct.unpack_from("<I", data, 0x3C)[0]
    if pe_offset + 24 > len(data):
        raise ValueError("PE 头不完整")
    if data[pe_offset : pe_offset + 4] != b"PE\0\0":
        raise ValueError("缺少 PE 文件头")
    machine = struct.unpack_from("<H", data, pe_offset + 4)[0]
    expected = expected_pe_machine()
    if expected is not None and machine != expected:
        raise ValueError(
            f"架构不匹配，包内是 {pe_machine_name(machine)}，当前需要 {pe_machine_name(expected)}"
        )
    section_count = struct.unpack_from("<H", data, pe_offset + 6)[0]
    if section_count == 0:
        raise ValueError("没有 PE 节")
    optional_header_size = struct.unpack_from("<H", data, pe_offset + 20)[0]
    optional_header_start = pe_offset + 24
    optional_header_end = optional_header_start + optional_header_size
    if optional_header_end > len(data):
        raise ValueError("可选头不完整")
    magic = struct.unpack_from("<H", data, optional_header_start)[0]
    if magic not in (0x010B, 0x020B):
        raise ValueError("可选头标记无效")
    section_table_end = optional_header_end + section_count * 40
    if section_table_end > len(data):
        raise ValueError("节表不完整")
    for i in range(section_count):
        offset = optional_header_end + i * 40
        raw_size = struct.unpack_from("<I", data, offset + 16)[0]
        raw_ptr = struct.unpack_from("<I", data, offset + 20)[0]
        if raw_size == 0:
            continue
        raw_end = raw_ptr + raw_size
        if raw_ptr == 0 or raw_end > len(data):
            raise ValueError(f"文件被截断，需要至少 {raw_end} 字节，实际 {len(data)} 字节")


def expected_mach_o_cputype() -> int | None:
    machine = platform.machine().lower()
    if machine in ("arm64", "aarch64"):
        return 0x0100000C
    if machine in ("x86_64", "amd64"):
        return 0x01000007
    return None


def validate_mach_o_image(data: bytes) -> None:
    if len(data) < 8:
        raise ValueError("文件太小")
    magic = struct.unpack_from("<I", data, 0)[0]
    MH_MAGIC_64, MH_CIGAM_64 = 0xFEEDFACF, 0xCFFAEDFE
    FAT_MAGIC, FAT_CIGAM = 0xCAFEBABE, 0xBEBAFECA
    MH_MAGIC, MH_CIGAM = 0xFEEDFACE, 0xCEFAEDFE
    if magic not in (MH_MAGIC_64, MH_CIGAM_64, FAT_MAGIC, FAT_CIGAM, MH_MAGIC, MH_CIGAM):
        magic_be = struct.unpack_from(">I", data, 0)[0]
        if magic_be not in (MH_MAGIC_64, MH_CIGAM_64, FAT_MAGIC, FAT_CIGAM, MH_MAGIC, MH_CIGAM):
            raise ValueError("缺少 Mach-O 文件头")
        magic = magic_be
    if magic in (FAT_MAGIC, FAT_CIGAM):
        return
    if len(data) < 12:
        raise ValueError("Mach-O 头不完整")
    if magic in (MH_CIGAM_64, MH_CIGAM):
        cputype = struct.unpack_from(">I", data, 4)[0]
    else:
        cputype = struct.unpack_from("<I", data, 4)[0]
    expected = expected_mach_o_cputype()
    if expected is not None and cputype != expected:
        raise ValueError(f"架构不匹配，包内 cputype=0x{cputype:x}，当前需要 0x{expected:x}")


def validate_update_zip(zip_path: Path, expected_name: str) -> None:
    with zipfile.ZipFile(zip_path) as zf:
        exe_name = next(
            (n for n in zf.namelist() if Path(n).name.lower() == expected_name.lower()),
            "",
        )
        if not exe_name:
            sys.exit(f"[FAIL] 更新包里没有 {expected_name}: {zip_path}")
        data = zf.read(exe_name)
        try:
            if is_windows():
                validate_pe_image(data)
            elif is_macos():
                validate_mach_o_image(data)
            elif not data:
                raise ValueError("文件为空")
        except ValueError as exc:
            sys.exit(f"[FAIL] 更新包里的 {expected_name} 无效: {exc}")


def bump(cur: str, args) -> str:
    if args.version:
        return args.version
    a, b, c = (cur.split(".") + ["0", "0"])[:3]
    if args.major:
        return f"{int(a) + 1}.0.0"
    if args.minor:
        return f"{a}.{int(b) + 1}.0"
    return f"{a}.{b}.{int(c) + 1}"


def set_version(path: Path, version: str) -> None:
    data = json.loads(path.read_text(encoding="utf-8"))
    data["version"] = version
    path.write_text(json.dumps(data, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def executable_version(exe: Path) -> str:
    if not is_windows():
        return ""
    literal = str(exe).replace("'", "''")
    command = f"(Get-Item -LiteralPath '{literal}').VersionInfo.ProductVersion"
    proc = subprocess.run(
        ["powershell.exe", "-NoProfile", "-NonInteractive", "-Command", command],
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    if proc.returncode != 0:
        sys.exit(f"[FAIL] 无法读取构建产物版本: {(proc.stderr or proc.stdout).strip()}")
    return proc.stdout.strip()


def main() -> None:
    channel = update_channel()
    bin_name = binary_name()
    ap = argparse.ArgumentParser(description="打包 Nova 更新 zip（GitHub Release 分发）")
    ap.add_argument("--version", "-v", default="", help="显式指定版本号（默认 patch 自增）")
    ap.add_argument("--minor", action="store_true", help="次版本自增 x.y.0")
    ap.add_argument("--major", action="store_true", help="主版本自增 x.0.0")
    ap.add_argument("--no-build", action="store_true", help="跳过构建，只改版本号并压缩现有产物")
    ap.add_argument("--no-upload", action="store_true", default=True, help="兼容旧参数（始终不上传）")
    ap.add_argument("--bump-only", action="store_true", help="只更新版本号后退出")
    ap.add_argument("--legacy", action="store_true", help="已废弃，无效果")
    args = ap.parse_args()

    cur = json.loads(CONF.read_text(encoding="utf-8"))["version"]
    new = bump(cur, args)
    print(f"版本: {cur} -> {new}")
    set_version(CONF, new)
    set_version(PKG, new)
    if args.bump_only:
        print(f"[OK] 已写入版本 {new}（bump-only）")
        return

    print(f"通道: {channel}  产物: {bin_name}")
    if not args.no_build:
        r = subprocess.run("npx tauri build --no-bundle", shell=True, cwd=ROOT)
        if r.returncode != 0:
            sys.exit(f"tauri build 失败（exit {r.returncode}）")

    exe = ROOT / "src-tauri" / "target" / "release" / bin_name
    if not exe.exists():
        sys.exit(f"未找到构建产物: {exe}")
    if not is_windows():
        exe.chmod(exe.stat().st_mode | 0o755)
    actual_version = executable_version(exe)
    if actual_version and actual_version != new:
        sys.exit(f"[FAIL] 构建产物版本不匹配: 计划 {new}，实际 {actual_version}")
    out_dir = ROOT / "release"
    out_dir.mkdir(exist_ok=True)
    zip_path = out_dir / f"{channel}-{new}.zip"
    with zipfile.ZipFile(zip_path, "w", zipfile.ZIP_DEFLATED) as zf:
        zf.write(exe, exe.name)
    validate_update_zip(zip_path, bin_name)
    print(f"\n[OK] 打包完成: {zip_path} ({zip_path.stat().st_size / 1048576:.1f} MB)")
    print("用 GitHub Actions Release 或 gh release upload 发布该 zip。")


if __name__ == "__main__":
    try:
        sys.stdout.reconfigure(encoding="utf-8")
    except Exception:
        pass
    main()
