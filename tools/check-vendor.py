#!/usr/bin/env python3
"""Guardrail: what gets committed under `vendor/` (and `variants/`) is source.

The repo builds vendored code at package time (meow, sshd, wireguard): a
committed binary silently pins one build and rots away from the source beside
it. Two places are allowed to hold a prebuilt binary, and both must carry a
SHA256SUMS next to it:

- `vendor/prebuilt/<triple>/` — a kernel module that needs the vendor kernel
  tree, which CI does not have;
- `plugins/<plugin>/variants/<unit>/` — a file only one package may carry
  (a device's kernel module, a backend only the universal package ships). The
  unit directory is staged into that package alone.

Only files git tracks are inspected: a local build leaves `.o`/`.a` inside
`vendor/` (sshd's does), and those are not what ships or what anyone reviews.
A brand-new plugin enters the check when its files are staged/committed.

Run from anywhere: `python3 tools/check-vendor.py`.
"""

from __future__ import annotations

import hashlib
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# Executable formats that must never be committed outside vendor/prebuilt/.
MAGIC = {
    b"\x7fELF": "ELF",
    b"MZ": "PE",
    b"\xcf\xfa\xed\xfe": "Mach-O",
    b"\xce\xfa\xed\xfe": "Mach-O",
    b"\xca\xfe\xba\xbe": "Mach-O/fat",
    b"!<arch>": "ar/static library",
}


# GitHub runners on Windows default to a cp1252 stdout; our messages and the
# ::error:: annotations are UTF-8. Reconfigure instead of dropping the detail.
for _stream in (sys.stdout, sys.stderr):
    try:
        _stream.reconfigure(encoding="utf-8", errors="replace")
    except AttributeError:  # pragma: no cover - Python < 3.7
        pass


def tracked_vendor_files() -> list[Path]:
    result = subprocess.run(
        ["git", "-C", str(ROOT), "ls-files", "-z", "--", "plugins/*/vendor/*"],
        capture_output=True,
        check=True,
    )
    names = [name for name in result.stdout.decode("utf-8", "replace").split("\0") if name]
    return [ROOT / name for name in names]


def format_of(path: Path) -> str | None:
    try:
        with path.open("rb") as handle:
            head = handle.read(8)
    except OSError:
        return None
    for magic, name in MAGIC.items():
        if head.startswith(magic):
            return name
    return None


def tracked_variant_files() -> list[Path]:
    result = subprocess.run(
        ["git", "-C", str(ROOT), "ls-files", "-z", "--", "plugins/*/variants/*"],
        capture_output=True,
        check=True,
    )
    names = [name for name in result.stdout.decode("utf-8", "replace").split("\0") if name]
    return [ROOT / name for name in names]


def sums_entries(sums: Path) -> list[tuple[str, str]]:
    """`sha256sum` format: `<hex>  <name>`, with the name relative to `sums`."""
    entries: list[tuple[str, str]] = []
    for line in sums.read_text(encoding="utf-8", errors="replace").splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        digest, _, name = line.partition(" ")
        entries.append((digest.strip().lower(), name.strip().lstrip("*")))
    return entries


def verify_sums(directory: Path, binaries: list[Path], problems: list[str]) -> None:
    """A `SHA256SUMS` has to resolve from the directory that holds it.

    `build.sh` checks it exactly that way (`cd <unit> && sha256sum -c`), so a
    name that only resolves from somewhere else — the bare file name a
    `vendor/prebuilt/<triple>/` manifest used to carry — would fail the release
    job at its last step instead of here. Every binary in the directory also has
    to be listed: an unlisted prebuilt file is one nobody can verify.
    """
    sums = directory / "SHA256SUMS"
    label = sums.relative_to(ROOT).as_posix()
    listed: set[str] = set()
    for digest, name in sums_entries(sums):
        listed.add(name)
        target = directory / name
        if not name or not target.is_file():
            problems.append(f"{label}: {name or '（空名字）'} 不存在（名字相对 SHA256SUMS 所在目录）")
            continue
        if hashlib.sha256(target.read_bytes()).hexdigest() != digest:
            problems.append(f"{label}: {name} 的 SHA256 与文件内容不符")
    for binary in binaries:
        name = binary.relative_to(directory).as_posix()
        if name not in listed:
            problems.append(f"{label}: {name} 没写进 SHA256SUMS（预编译件必须可校验）")


def main() -> int:
    try:
        files = tracked_vendor_files()
        variant_files = tracked_variant_files()
    except (OSError, subprocess.CalledProcessError) as error:
        print(f"check-vendor: 读不到 git 跟踪的文件（{error}）", file=sys.stderr)
        return 1

    problems: list[str] = []
    prebuilt_dirs: set[Path] = set()
    prebuilt_binaries: dict[Path, list[Path]] = {}
    for path in files:
        rel = path.relative_to(ROOT)
        vendor = next((parent for parent in rel.parents if parent.name == "vendor"), None)
        if vendor is None:
            continue
        inside = rel.relative_to(vendor)
        if inside.parts[:1] == ("prebuilt",):
            if len(inside.parts) >= 3:
                directory = ROOT / vendor / "prebuilt" / inside.parts[1]
                prebuilt_dirs.add(directory)
                if format_of(path):
                    prebuilt_binaries.setdefault(directory, []).append(path)
            continue
        kind = format_of(path)
        if kind:
            problems.append(
                f"{rel.as_posix()}: vendor/ 里提交了 {kind} 二进制；"
                "源码放 vendor/，预编译件只能放 vendor/prebuilt/<triple>/ 或 "
                "plugins/<插件>/variants/<unit>/ 并带 SHA256SUMS"
            )

    for directory in sorted(prebuilt_dirs):
        if not (directory / "SHA256SUMS").is_file():
            rel = directory.relative_to(ROOT).as_posix()
            problems.append(f"{rel}: 缺 SHA256SUMS（预编译件必须可校验）")
            continue
        verify_sums(directory, prebuilt_binaries.get(directory, []), problems)

    # `plugins/<plugin>/variants/<unit>/` 只进那个单元自己的包：里面允许有预编译件
    # （设备内核模块），但和 vendor/prebuilt 一样必须能校验。
    variant_dirs: set[Path] = set()
    variant_binaries: dict[Path, list[Path]] = {}
    for path in variant_files:
        rel = path.relative_to(ROOT)
        if not format_of(path):
            continue
        parts = rel.parts
        if len(parts) < 5 or parts[0] != "plugins" or parts[2] != "variants":
            problems.append(
                f"{rel.as_posix()}: 变体文件要在 plugins/<插件>/variants/<unit>/ 下"
            )
            continue
        directory = ROOT.joinpath(*parts[:4])
        variant_dirs.add(directory)
        variant_binaries.setdefault(directory, []).append(path)

    for directory in sorted(variant_dirs):
        if not (directory / "SHA256SUMS").is_file():
            rel = directory.relative_to(ROOT).as_posix()
            problems.append(f"{rel}: 缺 SHA256SUMS（变体里的预编译件必须可校验）")
            continue
        verify_sums(directory, variant_binaries.get(directory, []), problems)

    if problems:
        for problem in problems:
            print(f"::error::{problem}")
        return 1
    print(
        f"check-vendor: 检查了 {len(files)} 个 vendor 文件、{len(variant_files)} 个变体文件，"
        "源码/预编译件分界正常"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
