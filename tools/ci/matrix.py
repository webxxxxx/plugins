#!/usr/bin/env python3
"""Build the release matrix: one row per (plugin, platform[, device]) package.

The workflow used to carry this logic inline as bash+jq, which meant it could
only be exercised by pushing a tag. Extracted here so `test_matrix.py` covers
it and a local run reproduces CI exactly.

Rows come from `.plugin.json`:

  platforms   every package of that platform is built
  variants[]  a device-restricted package built *next to* the universal one
              (e.g. wireguard's `sr6110` kernel-module build on aarch64)

Usage (CI):
  python3 tools/ci/matrix.py --filter all --output "$GITHUB_OUTPUT"
  python3 tools/ci/matrix.py --filter changed \\
      --changed-files "$(gh api ... --jq '.[].filename')" --output "$GITHUB_OUTPUT"

`--output` writes GitHub's `key=value` lines; without it the JSON result is
printed instead, which is what the tests use.
"""

from __future__ import annotations

import argparse
import fnmatch
import json
import sys
from pathlib import Path

OS_BY_PLATFORM = {
    "*-apple-darwin": "macos-15",
    "*-windows-*": "windows-2025",
    "*-unknown-linux-gnu": "ubuntu-22.04",
}
DEFAULT_OS = "ubuntu-24.04"


# GitHub runners on Windows default to a cp1252 stdout; our messages and the
# ::error:: annotations are UTF-8. Reconfigure instead of dropping the detail.
for _stream in (sys.stdout, sys.stderr):
    try:
        _stream.reconfigure(encoding="utf-8", errors="replace")
    except AttributeError:  # pragma: no cover - Python < 3.7
        pass


def os_for(platform: str) -> str:
    for pattern, os_name in OS_BY_PLATFORM.items():
        if fnmatch.fnmatchcase(platform, pattern):
            return os_name
    return DEFAULT_OS


def changed_sets(files: list[str]) -> tuple[set[str], bool]:
    """Plugins touched by a PR, and whether a shared change forces the full matrix."""
    plugins: set[str] = set()
    build_all = False
    for name in files:
        if name.startswith("plugins/"):
            rest = name[len("plugins/"):]
            if "/" in rest:
                plugins.add(rest.split("/", 1)[0])
        if name.startswith(("sdk/", "templates/", "tools/cli/")):
            build_all = True
    return plugins, build_all


def rows_for(plugin_dir: Path, meta: dict, macos: bool) -> list[dict]:
    name = meta.get("name")
    if not name:
        return []
    platforms = (meta.get("ci") or {}).get("platforms") or meta.get("platforms") or []
    units: list[tuple[str, str]] = [(platform, "") for platform in platforms]
    for variant in meta.get("variants") or []:
        device = (variant.get("device") or "").strip()
        if not device:
            continue
        for platform in variant.get("platforms") or []:
            units.append((platform, device))
    rows = []
    for platform, device in units:
        os_name = os_for(platform)
        if os_name == "macos-15" and not macos:
            continue
        rows.append(
            {
                "plugin": name,
                "dir": f"plugins/{name}",
                "platform": platform,
                "device": device,
                "key": platform + (f"-{device}" if device else ""),
                "os": os_name,
            }
        )
    return rows


def build_matrix(root: Path, filter_name: str, files: list[str], macos: bool) -> dict:
    changed_plugins, build_all = changed_sets(files)
    rows: list[dict] = []
    matched = False
    for plugin_dir in sorted((root / "plugins").iterdir()):
        meta_path = plugin_dir / ".plugin.json"
        if not plugin_dir.is_dir() or not meta_path.is_file():
            continue
        name = plugin_dir.name
        if filter_name == "all":
            include = True
        elif filter_name == "changed":
            include = build_all or name in changed_plugins
        else:
            if name == filter_name:
                matched = True
            include = name == filter_name
        if not include:
            continue
        try:
            meta = json.loads(meta_path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as error:
            raise SystemExit(f"{meta_path}: 读不了（{error}）")
        rows.extend(rows_for(plugin_dir, meta, macos))

    if filter_name == "changed":
        has_plugins = bool(rows)
    else:
        if filter_name != "all" and not matched:
            raise SystemExit(f"unknown plugin '{filter_name}' (no plugins/{filter_name}/.plugin.json)")
        if not rows:
            raise SystemExit("no plugins with a .plugin.json were found")
        has_plugins = True
    return {"has_plugins": has_plugins, "matrix": {"include": rows}}


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", default=Path(__file__).resolve().parents[2], type=Path)
    parser.add_argument("--filter", default="all")
    parser.add_argument("--changed-files", default="")
    parser.add_argument("--macos", action="store_true")
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()

    files = [line.strip() for line in args.changed_files.splitlines() if line.strip()]
    result = build_matrix(args.root, args.filter, files, args.macos)

    if args.output:
        lines = [
            f"has_plugins={'true' if result['has_plugins'] else 'false'}",
            f"matrix={json.dumps(result['matrix'], separators=(',', ':'))}",
        ]
        args.output.write_text("\n".join(lines) + "\n", encoding="utf-8")
    else:
        json.dump(result, sys.stdout, indent=2, ensure_ascii=False)
        sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
