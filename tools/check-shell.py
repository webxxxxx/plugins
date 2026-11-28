#!/usr/bin/env python3
"""Guardrail: every shell script in the repo parses, and none carries CRLF.

The payload scripts run on the device under busybox sh and under `set -u`;
a CRLF line ending turns `set -u` into `set: illegal option -`, and a syntax
slip only shows up when the plugin is installed. Both are cheap to catch here.

Run from anywhere: `python3 tools/check-shell.py`.

Excluded trees:
  vendor/       upstream sources, not ours to reformat
  target/       cargo output
  node_modules/ .git/ dist/ .scaffold/
"""

from __future__ import annotations

import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SKIP_DIRS = {"vendor", "target", "node_modules", ".git", "dist", ".scaffold", "out"}


# GitHub runners on Windows default to a cp1252 stdout; our messages and the
# ::error:: annotations are UTF-8. Reconfigure instead of dropping the detail.
for _stream in (sys.stdout, sys.stderr):
    try:
        _stream.reconfigure(encoding="utf-8", errors="replace")
    except AttributeError:  # pragma: no cover - Python < 3.7
        pass


def scripts() -> list[Path]:
    found: list[Path] = []
    for path in ROOT.rglob("*.sh"):
        if any(part in SKIP_DIRS for part in path.relative_to(ROOT).parts):
            continue
        found.append(path)
    return sorted(found)


def checker_for(path: Path) -> str | None:
    """A bash script is checked with bash, everything else with dash/sh."""
    try:
        first = path.read_bytes().split(b"\n", 1)[0].decode("utf-8", "replace")
    except OSError:
        return None
    if "bash" in first:
        return shutil.which("bash") or shutil.which("sh")
    return shutil.which("dash") or shutil.which("sh")


def tracked_modes() -> dict[str, str]:
    """Index modes, not worktree ones: on Windows every file looks 644."""
    result = subprocess.run(
        ["git", "-C", str(ROOT), "ls-files", "-s", "--", "*.sh"],
        capture_output=True,
        check=True,
    )
    modes: dict[str, str] = {}
    for line in result.stdout.decode("utf-8", "replace").splitlines():
        meta, _, path = line.partition("\t")
        if not path:
            continue
        modes[path] = meta.split()[0]
    return modes


def payload_entries() -> list[str]:
    """Tracked scripts that a package ships under `payload/bin/`.

    A package's file modes come from the source tree, and the release pipeline
    only ever tightens them (`umask 077` while signing). These scripts are the
    ones another script may run directly (`"$HERE/wireguard.sh" up`), so the
    executable bit has to be in git — a Windows checkout cannot supply it.
    """
    found = []
    for path in ROOT.rglob("*.sh"):
        parts = path.relative_to(ROOT).parts
        if not parts or parts[0] != "plugins":
            continue
        if any(part in SKIP_DIRS for part in parts):
            continue
        if len(parts) >= 2 and parts[-2] == "bin" and "payload" in parts:
            found.append(path.relative_to(ROOT).as_posix())
    return sorted(found)


def main() -> int:
    if not (shutil.which("dash") or shutil.which("sh")):
        print("check-shell: 找不到 dash/sh，无法做语法检查", file=sys.stderr)
        return 1

    problems: list[str] = []
    modes = tracked_modes()
    for rel in payload_entries():
        mode = modes.get(rel)
        if mode is None:  # untracked scratch file: not what ships
            continue
        if mode != "100755":
            problems.append(
                f"{rel}: 在 git 里不是可执行（{mode}）；包里的模式来自源文件，"
                f"脚本之间直接调用会 EACCES——git update-index --chmod=+x {rel}"
            )
    for path in scripts():
        rel = path.relative_to(ROOT).as_posix()
        data = path.read_bytes()
        if b"\r\n" in data:
            problems.append(f"{rel}: 有 CRLF 行尾（payload 脚本必须是 LF）")
        checker = checker_for(path)
        if not checker:
            problems.append(f"{rel}: 找不到解释器做语法检查")
            continue
        try:
            result = subprocess.run(
                [checker, "-n", str(path)],
                capture_output=True,
                text=True,
                timeout=30,
            )
        except (OSError, subprocess.TimeoutExpired) as error:
            problems.append(f"{rel}: 语法检查跑不起来（{error}）")
            continue
        if result.returncode != 0:
            detail = (result.stderr or result.stdout).strip().replace("\n", " ")
            problems.append(f"{rel}: 语法错误（{detail}）")

    if problems:
        for problem in problems:
            print(f"::error::{problem}")
        return 1
    print(f"check-shell: {len(scripts())} 个脚本，语法与行尾都正常")
    return 0


if __name__ == "__main__":
    sys.exit(main())
