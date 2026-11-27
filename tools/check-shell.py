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


def scripts() -> list[Path]:
    found: list[Path] = []
    for path in ROOT.rglob("*.sh"):
        if any(part in SKIP_DIRS for part in path.relative_to(ROOT).parts):
            continue
        found.append(path)
    return sorted(found)


def main() -> int:
    checker = shutil.which("dash") or shutil.which("sh")
    if not checker:
        print("check-shell: 找不到 dash/sh，无法做语法检查", file=sys.stderr)
        return 1

    problems: list[str] = []
    for path in scripts():
        rel = path.relative_to(ROOT).as_posix()
        data = path.read_bytes()
        if b"\r\n" in data:
            problems.append(f"{rel}: 有 CRLF 行尾（payload 脚本必须是 LF）")
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
