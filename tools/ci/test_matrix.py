#!/usr/bin/env python3
"""Tests for tools/ci/matrix.py — the release matrix must be verifiable locally."""

from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

import matrix


def plugin(root: Path, name: str, body: dict) -> None:
    directory = root / "plugins" / name
    directory.mkdir(parents=True)
    (directory / ".plugin.json").write_text(json.dumps(body), encoding="utf-8")


class MatrixTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        plugin(
            self.root,
            "wg",
            {
                "name": "wg",
                "platforms": ["x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl"],
                "variants": [{"device": "sr6110", "platforms": ["aarch64-unknown-linux-musl"]}],
            },
        )
        plugin(
            self.root,
            "desk",
            {"name": "desk", "platforms": ["x86_64-pc-windows-msvc", "aarch64-apple-darwin"]},
        )

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def test_all_platforms_plus_variants(self) -> None:
        result = matrix.build_matrix(self.root, "all", [], macos=False)
        keys = [row["key"] for row in result["matrix"]["include"]]
        self.assertTrue(result["has_plugins"])
        # rows are grouped by plugin directory, sorted by name ("desk" < "wg")
        self.assertEqual(
            keys,
            [
                "x86_64-pc-windows-msvc",
                "x86_64-unknown-linux-musl",
                "aarch64-unknown-linux-musl",
                "aarch64-unknown-linux-musl-sr6110",
            ],
        )

    def test_macos_rows_need_the_flag(self) -> None:
        without = matrix.build_matrix(self.root, "all", [], macos=False)
        self.assertNotIn("aarch64-apple-darwin", [r["platform"] for r in without["matrix"]["include"]])
        with_macos = matrix.build_matrix(self.root, "all", [], macos=True)
        self.assertIn("macos-15", [r["os"] for r in with_macos["matrix"]["include"]])

    def test_os_mapping(self) -> None:
        rows = matrix.build_matrix(self.root, "all", [], macos=True)["matrix"]["include"]
        by_platform = {row["platform"]: row["os"] for row in rows}
        self.assertEqual(by_platform["x86_64-pc-windows-msvc"], "windows-2025")
        self.assertEqual(by_platform["aarch64-apple-darwin"], "macos-15")

    def test_changed_filters_to_touched_plugin(self) -> None:
        result = matrix.build_matrix(self.root, "changed", ["plugins/desk/package/manifest.json"], False)
        self.assertEqual({row["plugin"] for row in result["matrix"]["include"]}, {"desk"})

    def test_shared_change_falls_back_to_everything(self) -> None:
        result = matrix.build_matrix(self.root, "changed", ["tools/cli/src/lib.rs"], False)
        self.assertEqual({row["plugin"] for row in result["matrix"]["include"]}, {"wg", "desk"})

    def test_changed_with_nothing_touched_is_empty_not_an_error(self) -> None:
        result = matrix.build_matrix(self.root, "changed", ["README.md"], False)
        self.assertFalse(result["has_plugins"])
        self.assertEqual(result["matrix"]["include"], [])

    def test_unknown_plugin_is_an_error(self) -> None:
        with self.assertRaises(SystemExit):
            matrix.build_matrix(self.root, "nope", [], False)

    def test_plugin_without_platforms_contributes_nothing(self) -> None:
        plugin(self.root, "empty", {"name": "empty"})
        result = matrix.build_matrix(self.root, "all", [], False)
        self.assertNotIn("empty", {row["plugin"] for row in result["matrix"]["include"]})


if __name__ == "__main__":
    unittest.main()
