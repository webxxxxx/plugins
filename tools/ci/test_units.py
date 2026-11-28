#!/usr/bin/env python3
"""`plugin-tools units` and the CI matrix must agree.

The unit list lives in `.plugin.json` and is read from more than one place:
`build` (and the `units` verb) in Rust, `pack.sh` through `units`, and
`tools/ci/matrix.py`, which has to compute it before anything is built. This test
pins the two implementations together — the same (plugin, platform, device)
triples, no more and no less — so a rule added on one side (a narrower
`ci.platforms`, a new variant) cannot silently make the release build a package
nobody expects.

Skips when the binary has not been built yet; CI runs it after that step.
"""

from __future__ import annotations

import json
import subprocess
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(Path(__file__).resolve().parent))

import matrix  # noqa: E402

BINARIES = (
    (
        ROOT / "tools/cli/target/debug/plugin-tools.exe",
        ROOT / "tools/cli/target/debug/plugin-tools",
    )
    if sys.platform == "win32"
    else (ROOT / "tools/cli/target/debug/plugin-tools",)
)


def plugin_tools() -> Path | None:
    return next((path for path in BINARIES if path.is_file()), None)


class UnitsAgree(unittest.TestCase):
    def test_matrix_matches_plugin_tools_units(self) -> None:
        tools = plugin_tools()
        if tools is None:
            self.skipTest(
                "plugin-tools 还没编：cargo build --manifest-path tools/cli/Cargo.toml"
            )
        # macOS rows are filtered out of the matrix unless asked for; `units`
        # reports every declared platform, so ask for the whole picture.
        rows = matrix.build_matrix(ROOT, "all", [], True)["matrix"]["include"]
        expected = {(row["plugin"], row["platform"], row["device"]) for row in rows}
        actual: set[tuple[str, str, str]] = set()
        for plugin_dir in sorted((ROOT / "plugins").iterdir()):
            if not (plugin_dir / ".plugin.json").is_file():
                continue
            output = subprocess.run(
                [str(tools), "units", "--dir", str(plugin_dir), "--json"],
                capture_output=True,
                text=True,
                check=True,
            ).stdout
            for row in json.loads(output):
                actual.add((row["plugin"], row["platform"], row["device"] or ""))
        self.assertEqual(sorted(expected), sorted(actual))


if __name__ == "__main__":
    unittest.main()
