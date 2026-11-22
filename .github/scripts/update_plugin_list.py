#!/usr/bin/env python3
"""Generate docs/plugins.md from the latest stable GitHub release assets.

The page lists the newest .spk for every plugin/platform/device triple. A
per-plugin release therefore only replaces that plugin's rows, while other
plugins keep their current build.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
import urllib.error
import urllib.request
from pathlib import Path

ASSET_RE = re.compile(
    r"^(?P<plugin>.+?)-(?P<version>\d+\.\d+\.\d+(?:-[0-9A-Za-z.]+)?)-"
    r"(?P<platform>(?:x86_64|aarch64|armv7|i686|universal2)-"
    r"(?:unknown-linux-(?:gnu|musl(?:eabihf)?)|apple-darwin|pc-windows-(?:msvc|gnu)))"
    r"(?:-(?P<device>[a-z0-9][a-z0-9._-]*))?\.spk$"
)


def api_get(url: str, token: str | None) -> object:
    headers = {
        "Accept": "application/vnd.github+json",
        "User-Agent": "plugin-list-updater",
    }
    if token:
        headers["Authorization"] = f"Bearer {token}"
    request = urllib.request.Request(url, headers=headers)
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return json.load(response)
    except urllib.error.HTTPError as exc:
        body = exc.read().decode("utf-8", "replace")
        raise SystemExit(f"GitHub API {exc.code} for {url}: {body}") from exc
    except urllib.error.URLError as exc:
        raise SystemExit(f"could not reach GitHub API: {exc}") from exc


def iter_releases(repo: str, token: str | None):
    page = 1
    while True:
        url = f"https://api.github.com/repos/{repo}/releases?per_page=100&page={page}"
        batch = api_get(url, token)
        if not isinstance(batch, list):
            raise SystemExit(f"unexpected GitHub API response for {url}")
        yield from batch
        if len(batch) < 100:
            return
        page += 1


def parse_asset(name: str) -> dict[str, str] | None:
    match = ASSET_RE.match(name)
    return match.groupdict() if match else None


def human_size(size: int) -> str:
    return f"{size / (1024 * 1024):.1f} MiB"


def render(repo: str, latest: dict[tuple[str, str, str], dict[str, object]]) -> str:
    rows = sorted(
        latest.values(),
        key=lambda row: (
            str(row["plugin"]),
            str(row["platform"]),
            str(row.get("device") or ""),
        ),
    )
    if not rows:
        raise SystemExit("no stable .spk assets were found")

    updated = max(str(row["published_at"]) for row in rows)[:10] or "未知"
    release_url = f"https://github.com/{repo}/releases"
    lines = [
        "# 插件下载",
        "",
        "> 本文件由 CI 自动生成，请勿手改。只列每个插件/平台/设备的最新稳定版；"
        f"预发布和历史版本见 [Releases]({release_url})。",
        ">",
        "> 安装前先信任发布公钥：[docs/release-key.pub](release-key.pub)。",
        "",
        f"最后更新：{updated} · 共 {len(rows)} 个包",
        "",
        "| 插件 | 版本 | 平台 | 大小 | 下载地址 |",
        "| --- | --- | --- | ---: | --- |",
    ]

    for row in rows:
        name = str(row["name"])
        url = str(row["url"])
        lines.append(
            f"| {row['plugin']} | {row['version']} | {row['platform']} | "
            f"{human_size(int(row['size']))} | [{name}]({url}) |"
        )

    lines.append("")
    return "\n".join(lines)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", nargs="?", default="docs/plugins.md")
    parser.add_argument(
        "--repo",
        default=os.environ.get("GH_REPO") or os.environ.get("GITHUB_REPOSITORY"),
        help="GitHub repository, for example webxxxxx/plugins",
    )
    args = parser.parse_args()

    if not args.repo:
        raise SystemExit("--repo (or GH_REPO/GITHUB_REPOSITORY) is required")

    token = os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN")
    latest: dict[tuple[str, str, str], dict[str, object]] = {}

    for release in iter_releases(args.repo, token):
        if release.get("draft") or release.get("prerelease"):
            continue
        published_at = str(release.get("published_at") or "")
        tag = str(release.get("tag_name") or "")
        for asset in release.get("assets") or []:
            name = str(asset.get("name") or "")
            if not name.endswith(".spk"):
                continue
            parsed = parse_asset(name)
            if parsed is None:
                print(f"warning: cannot parse asset name: {name}", file=sys.stderr)
                continue
            key = (parsed["plugin"], parsed["platform"], parsed.get("device") or "")
            row = {
                **parsed,
                "tag": tag,
                "published_at": published_at,
                "name": name,
                "device": parsed.get("device") or "",
                "size": int(asset.get("size") or 0),
                "url": str(asset.get("browser_download_url") or ""),
            }
            if key not in latest or published_at > str(latest[key]["published_at"]):
                latest[key] = row

    output = Path(args.output)
    output.parent.mkdir(parents=True, exist_ok=True)
    tmp = output.with_name(output.name + ".tmp")
    tmp.write_text(render(args.repo, latest), encoding="utf-8", newline="\n")
    os.replace(tmp, output)
    print(f"wrote {output} ({len(latest)} packages)")


if __name__ == "__main__":
    main()