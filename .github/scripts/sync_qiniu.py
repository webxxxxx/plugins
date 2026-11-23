#!/usr/bin/env python3
"""Publish the build artifacts to Qiniu Kodo, newest first.

The bucket root is "what a fresh install should see":

    <plugin>-<version>-<platform>[-<device>].spk
    plugin-tools-<version>-<target>.tar.gz|zip
    signing.pub, release-key.pub
    SHA256SUMS          sums of the release that last wrote the root
    index.json          the navigation file: every root package plus its sums
    history/<version>/  files this release replaced

Three rules keep that shape honest:

  * a file is uploaded to the root only for the identity it belongs to —
    (plugin, platform, device) for a package, (tools, target) for the toolchain;
  * a root file is moved to history/<its version>/ only when this release
    carries a file with the same identity and a version that is not older, so a
    per-plugin release never evicts another plugin's current package;
  * `index.json` is rebuilt from the bucket listing, not from the local
    directory, so it cannot claim a file the bucket does not have.

The script only talks to Qiniu when it has credentials; `--dry-run` prints the
plan and the would-be index and touches nothing.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import sys
import tempfile
import urllib.error
import urllib.request
from datetime import datetime, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

# The package name grammar lives in one place; a second copy would drift from
# the catalog generator that feeds docs/plugins.md.
from update_plugin_list import parse_asset  # noqa: E402

INDEX_KEY = "index.json"
SUMS_KEY = "SHA256SUMS"
KEY_FILES = ("signing.pub", "release-key.pub")
HISTORY_PREFIX = "history/"
# Never moved to history: they are not versions, they describe the root itself.
ROOT_METADATA = {INDEX_KEY, SUMS_KEY, *KEY_FILES}

TOOLS_RE = re.compile(
    r"^plugin-tools-(?P<version>\d+\.\d+\.\d+(?:-[0-9A-Za-z.]+)?)-"
    r"(?P<target>(?:x86_64|aarch64|armv7|i686|universal2)-"
    r"(?:unknown-linux-(?:gnu|musl(?:eabihf)?)|apple-darwin|pc-windows-(?:msvc|gnu)))"
    r"\.(?:tar\.gz|zip)$"
)

SVCD_RE = re.compile(
    r"^svcd-(?P<version>\d+\.\d+\.\d+(?:-[0-9A-Za-z.]+)?)-"
    r"(?P<target>(?:x86_64|aarch64|armv7|i686|universal2)-"
    r"(?:unknown-linux-(?:gnu|musl(?:eabihf)?)|apple-darwin|pc-windows-(?:msvc|gnu)))"
    r"\.(?:zip|tar\.gz)$"
)

MIME_TYPES = {
    ".spk": "application/octet-stream",
    ".pub": "text/plain; charset=utf-8",
    ".json": "application/json; charset=utf-8",
    ".zip": "application/zip",
    ".sha256": "text/plain; charset=utf-8",
}


# --- the plan -------------------------------------------------------------


def is_root_key(key: str) -> bool:
    """A key that belongs to the flat root, not to `history/` or anything else.

    The listing already asks for `delimiter=/`, but a key that names a folder
    must never be mistaken for a package: `parse_asset` would happily read
    `history/1.0.1/meow-...` as the plugin `history/1.0.1/meow`.
    """
    return bool(key) and "/" not in key and not key.endswith("/")


def identity(key: str) -> tuple[str, ...] | None:
    """What a root file is the current version *of*, or None when unknown."""
    package = parse_asset(key)
    if package is not None:
        return ("package", package["plugin"], package["platform"], package.get("device") or "")
    tools = TOOLS_RE.match(key)
    if tools is not None:
        return ("tools", tools.group("target"))
    svcd = SVCD_RE.match(key)
    if svcd is not None:
        return ("svcd", svcd.group("target"))
    return None


def version_key(version: str) -> tuple[tuple[int, ...], int, str]:
    """Order stable versions above prereleases, numerically per component."""
    base, _, prerelease = version.partition("-")
    numbers = tuple(int(part) if part.isdigit() else 0 for part in base.split("."))
    numbers = numbers[:4] + (0,) * max(0, 4 - len(numbers))
    return (numbers, 0 if prerelease else 1, prerelease)


def plan_moves(
    root_keys: list[str], uploads: list[dict], tag: str
) -> tuple[list[tuple[str, str]], list[str]]:
    """Decide which root files the uploads replace.

    Returns (moves, leftovers): moves is a list of (source key, history key),
    leftovers is every root key the uploads do not touch. History is keyed by
    the release that replaced the file, so every directory under `history/` is
    one tag — the same shape as the `SHA256SUMS` archived beside it.
    """
    incoming: dict[tuple[str, ...], str] = {}
    for upload in uploads:
        ident = identity(upload["key"])
        if ident is None:
            continue
        previous = incoming.get(ident)
        if previous is None or version_key(upload["version"]) >= version_key(previous):
            incoming[ident] = upload["version"]

    moves: list[tuple[str, str]] = []
    leftovers: list[str] = []
    uploaded = {upload["key"] for upload in uploads}
    for key in sorted(root_keys):
        if not is_root_key(key) or key in ROOT_METADATA or key in uploaded:
            continue
        ident = identity(key)
        if ident is None:
            leftovers.append(key)
            continue
        current = incoming.get(ident)
        if current is None:
            leftovers.append(key)
            continue
        if version_key(current) < version_key(parse_version(key)):
            # A stale release is being re-run; keep what the root already has.
            leftovers.append(key)
            continue
        moves.append((key, history_key(key, tag)))
    return moves, leftovers


def stale_uploads(root_keys: list[str], uploads: list[dict]) -> list[str]:
    """Uploads that would put an older file next to a newer one at the root."""
    current: dict[tuple[str, ...], str] = {}
    for key in root_keys:
        if not is_root_key(key):
            continue
        ident = identity(key)
        if ident is not None:
            current[ident] = max(current.get(ident, "0"), parse_version(key), key=version_key)
    stale = []
    for upload in uploads:
        ident = identity(upload["key"])
        held = current.get(ident) if ident is not None else None
        if held is not None and version_key(upload["version"]) < version_key(held):
            stale.append(f"{upload['key']} is older than {held}, which the root already holds")
    return stale


def parse_version(key: str) -> str:
    package = parse_asset(key)
    if package is not None:
        return package["version"]
    tools = TOOLS_RE.match(key)
    if tools is not None:
        return tools.group("version")
    svcd = SVCD_RE.match(key)
    return svcd.group("version") if svcd is not None else "0.0.0"


def history_key(key: str, tag: str) -> str:
    """`history/<发布 tag>/<原文件名>`.

    Keying by tag keeps one scheme: the files a release replaced sit beside that
    release's `SHA256SUMS`, and a full release — whose plugins carry different
    versions — still gets exactly one directory.
    """
    return f"{HISTORY_PREFIX}{tag}/{key}"


def render_index(
    root_items: list[dict],
    domain: str,
    repo: str,
    sha256: dict[str, dict],
) -> str:
    """Build the navigation file from what the bucket actually holds."""
    base = domain.rstrip("/")
    packages: list[dict] = []
    tools: list[dict] = []
    svcd: list[dict] = []
    keys: list[dict] = []
    for item in root_items:
        key = str(item.get("key") or "")
        if not is_root_key(key) or key in (INDEX_KEY, SUMS_KEY):
            continue
        size = int(item.get("fsize") or 0)
        released = iso_time(item.get("putTime"))
        known = sha256.get(key)
        if known is not None and int(known.get("size") or 0) != size:
            known = None
        digest = (known or {}).get("sha256")
        package = parse_asset(key)
        if package is not None:
            packages.append(
                {
                    "plugin": package["plugin"],
                    "version": package["version"],
                    "platform": package["platform"],
                    "device": package.get("device") or None,
                    "file": key,
                    "size": size,
                    "sha256": digest,
                    "url": f"{base}/{key}",
                    "released": released,
                }
            )
            continue
        matched = TOOLS_RE.match(key)
        if matched is not None:
            tools.append(
                {
                    "version": matched.group("version"),
                    "target": matched.group("target"),
                    "file": key,
                    "size": size,
                    "sha256": digest,
                    "url": f"{base}/{key}",
                    "released": released,
                }
            )
            continue
        daemon = SVCD_RE.match(key)
        if daemon is not None:
            svcd.append(
                {
                    "version": daemon.group("version"),
                    "target": daemon.group("target"),
                    "file": key,
                    "size": size,
                    "sha256": digest,
                    "url": f"{base}/{key}",
                    "released": released,
                }
            )
            continue
        keys.append({"file": key, "size": size, "sha256": digest, "url": f"{base}/{key}"})

    packages.sort(key=lambda row: (row["plugin"], row["platform"], row["device"] or "", version_key(row["version"])))
    tools.sort(key=lambda row: (row["target"], version_key(row["version"])))
    svcd.sort(key=lambda row: row["target"])
    keys.sort(key=lambda row: row["file"])
    document = {
        "schema": 1,
        "generated_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "repo": f"https://github.com/{repo}",
        "base_url": base,
        "history_prefix": HISTORY_PREFIX,
        "sums_url": f"{base}/{SUMS_KEY}",
        "packages": packages,
        "tools": tools,
        "svcd": svcd,
        # The root checksum list covers every file we know a hash for, so the
        # host can verify an svcd archive with the same line format it already
        # understands.
        "svcd_sums_url": f"{base}/{SUMS_KEY}",
        "keys": keys,
    }
    return json.dumps(document, ensure_ascii=False, indent=2, sort_keys=False) + "\n"


def iso_time(put_time: object) -> str | None:
    try:
        stamp = int(put_time) / 10_000_000  # Kodo reports putTime in 100 ns units
    except (TypeError, ValueError):
        return None
    if stamp <= 0:
        return None
    return datetime.fromtimestamp(stamp, tz=timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


# --- the artifacts --------------------------------------------------------


def collect_uploads(directory: Path) -> list[dict]:
    """Every file in `directory` that belongs at the root, with its digest."""
    uploads: list[dict] = []
    for path in sorted(directory.iterdir()):
        if not path.is_file():
            continue
        name = path.name
        if name == SUMS_KEY:
            continue
        if name in KEY_FILES:
            uploads.append({"key": name, "path": path, "version": "", "size": path.stat().st_size})
            continue
        package = parse_asset(name)
        if package is not None:
            uploads.append(
                {
                    "key": name,
                    "path": path,
                    "version": package["version"],
                    "plugin": package["plugin"],
                    "platform": package["platform"],
                    "device": package.get("device") or "",
                    "size": path.stat().st_size,
                }
            )
            continue
        tools = TOOLS_RE.match(name)
        if tools is not None:
            uploads.append(
                {"key": name, "path": path, "version": tools.group("version"), "target": tools.group("target"), "size": path.stat().st_size}
            )
            continue
        daemon = SVCD_RE.match(name)
        if daemon is not None:
            uploads.append(
                {
                    "key": name,
                    "path": path,
                    "version": daemon.group("version"),
                    "target": daemon.group("target"),
                    "size": path.stat().st_size,
                }
            )
            continue
        print(
            f"skip {name}: not a package, toolchain archive, daemon archive or key",
            file=sys.stderr,
        )
    return prune_versions(uploads)


def prune_versions(uploads: list[dict]) -> list[dict]:
    """One version per identity per run.

    A build directory should hold exactly one build of each package. When it
    does not — a hand-copied file left behind — the newer one is what the root
    should get, and the older one is treated as superseded: it is not uploaded
    next to its successor, so `plan_moves` moves it to history instead.
    """
    kept: dict[tuple[str, ...], dict] = {}
    for upload in uploads:
        ident = identity(upload["key"])
        if ident is None:
            kept[("file", upload["key"])] = upload
            continue
        previous = kept.get(ident)
        if previous is None:
            kept[ident] = upload
            continue
        if version_key(upload["version"]) > version_key(previous["version"]):
            print(
                f"skip {previous['key']}: {upload['key']} is newer in this run",
                file=sys.stderr,
            )
            kept[ident] = upload
        else:
            print(
                f"skip {upload['key']}: {previous['key']} is newer in this run",
                file=sys.stderr,
            )
    return list(kept.values())


def sha256_of(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def render_sums(root_items: list[dict], digests: dict[str, dict]) -> str:
    """Checksums for everything at the root we know the hash of.

    The host looks a line up by exact file name, so this list has to cover the
    files a release did not touch as well — `svcd-<version>-<target>.zip` is
    downloaded and checked against it. A file whose hash we never learned is
    left out instead of guessed.
    """
    lines: list[str] = []
    for item in root_items:
        key = str(item.get("key") or "")
        if not is_root_key(key) or key == SUMS_KEY:
            continue
        known = digests.get(key)
        if known is None:
            continue
        if int(known.get("size") or 0) != int(item.get("fsize") or 0):
            continue
        lines.append(f"{known['sha256']}  {key}")
    return "\n".join(sorted(lines)) + "\n"


def mime_type(key: str) -> str:
    if key == SUMS_KEY:
        return "text/plain; charset=utf-8"
    if key.endswith(".tar.gz"):
        return "application/gzip"
    return MIME_TYPES.get(Path(key).suffix, "application/octet-stream")


# --- the transport --------------------------------------------------------


class Kodo:
    """The three Kodo calls this script needs, behind one lazy import."""

    def __init__(self, access_key: str, secret_key: str, bucket: str) -> None:
        try:
            import qiniu  # noqa: PLC0415 - optional so --dry-run needs no SDK
        except ImportError as error:  # pragma: no cover - only hit in CI
            raise SystemExit("the qiniu SDK is required: pip install qiniu==7.18.0") from error
        self.qiniu = qiniu
        self.auth = qiniu.Auth(access_key, secret_key)
        self.bucket = bucket
        self.manager = qiniu.BucketManager(self.auth)

    def list_root(self) -> list[dict]:
        items: list[dict] = []
        marker = None
        while True:
            ret, eof, info = self.manager.list(self.bucket, prefix="", marker=marker, limit=1000, delimiter="/")
            if not info.ok():
                raise SystemExit(f"list failed: HTTP {info.status_code} {ret}")
            items.extend(ret.get("items") or [])
            marker = ret.get("marker")
            if eof or not marker:
                return items

    def move(self, source: str, target: str) -> None:
        ret, info = self.manager.move(self.bucket, source, self.bucket, target, force="true")
        if not info.ok():
            raise SystemExit(f"move {source} -> {target} failed: HTTP {info.status_code} {ret}")

    def upload(self, path: Path, key: str) -> None:
        # `detectMime: -1` is the only value that keeps our Content-Type
        # verbatim: at 0 Kodo ignores `application/octet-stream` on purpose and
        # falls back to the extension, which turns a `.spk` into
        # "application/x-tar" — wrong, and a hint to intermediaries that these
        # bytes are an archive to unpack.
        token = self.auth.upload_token(self.bucket, key, 3600, policy={"detectMime": -1})
        ret, info = self.qiniu.put_file(token, key, str(path), mime_type=mime_type(key))
        if not info.ok():
            raise SystemExit(f"upload {key} failed: HTTP {info.status_code} {ret}")

    def refresh(self, urls: list[str]) -> str | None:
        """Drop the CDN's copy of the two short-lived files.

        Kodo's edge caches an object for a year by default (`Cache-Control:
        max-age=31536000`), so a rewritten `index.json` would keep serving the
        previous release's list until it expires — and the store, seeing an
        older list than GitHub's, would silently fall back to GitHub. Returns an
        error string when the refresh did not take.
        """
        manager = self.qiniu.CdnManager(self.auth)
        _, info = manager.refresh_urls(urls)
        if info.ok():
            return None
        return f"HTTP {info.status_code} {info.text_body[:200]}"

    def fetch(self, url: str) -> str | None:
        try:
            with urllib.request.urlopen(url, timeout=20) as response:
                return response.read().decode("utf-8")
        except (urllib.error.URLError, OSError):
            return None


# --- wiring ---------------------------------------------------------------


def load_listing(path: Path) -> list[dict]:
    document = json.loads(path.read_text(encoding="utf-8"))
    if isinstance(document, dict):
        document = document.get("items") or []
    return [{"key": str(item["key"]), **item} for item in document]


def load_index(path: Path) -> dict[str, dict]:
    try:
        document = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return {}
    return index_digests(document)


def index_digests(document: dict) -> dict[str, dict]:
    digests: dict[str, dict] = {}
    for section in ("packages", "tools", "svcd", "keys"):
        for row in document.get(section) or []:
            name = row.get("file")
            if name and row.get("sha256"):
                digests[name] = {"sha256": row["sha256"], "size": int(row.get("size") or 0)}
    return digests


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--dir", default="dist", help="directory holding this release's artifacts")
    parser.add_argument("--tag", required=True, help="release tag, for example v0.3.0")
    parser.add_argument("--bucket", default=os.environ.get("QINIU_BUCKET"))
    parser.add_argument(
        "--domain",
        default=os.environ.get("QINIU_DOMAIN"),
        help="public base URL of the bucket (http or https; http is allowed but warned about)",
    )
    parser.add_argument("--repo", default=os.environ.get("GITHUB_REPOSITORY", "webxxxxx/plugins"))
    parser.add_argument("--dry-run", action="store_true", help="print the plan and the index, change nothing")
    parser.add_argument("--listing", type=Path, help="fixture for --dry-run: the current root listing")
    parser.add_argument("--previous-index", type=Path, help="fixture for --dry-run: the current index.json")
    parser.add_argument("--print-index", action="store_true", help="print the index and exit")
    args = parser.parse_args(argv)

    directory = Path(args.dir)
    if not directory.is_dir():
        raise SystemExit(f"no such directory: {directory}")
    uploads = collect_uploads(directory)
    if not uploads:
        raise SystemExit(f"no artifacts found in {directory}")
    for upload in uploads:
        upload["sha256"] = sha256_of(upload["path"])

    domain = (args.domain or "https://example.invalid").rstrip("/")
    if args.domain and not domain.startswith(("https://", "http://")):
        raise SystemExit("QINIU_DOMAIN must be the http:// or https:// base URL of the bucket")
    if not args.dry_run and domain.startswith("http://"):
        # The store accepts this when `qiniu_base` uses http too, but the index
        # and every package then travel in plaintext: say so once, loudly.
        print(
            f"warning: QINIU_DOMAIN is {domain} — index.json and the packages will be served "
            "over plaintext; a certificate on the bucket domain is the better fix",
            file=sys.stderr,
        )

    if args.dry_run:
        root_items = load_listing(args.listing) if args.listing else []
        previous = load_index(args.previous_index) if args.previous_index else {}
    else:
        if not args.bucket:
            raise SystemExit("QINIU_BUCKET (or --bucket) is required")
        access_key = os.environ.get("QINIU_ACCESS_KEY")
        secret_key = os.environ.get("QINIU_SECRET_KEY")
        if not access_key or not secret_key:
            raise SystemExit("QINIU_ACCESS_KEY and QINIU_SECRET_KEY are required")
        kodo = Kodo(access_key, secret_key, args.bucket)
        root_items = kodo.list_root()
        previous = index_digests(json.loads(kodo.fetch(f"{domain}/{INDEX_KEY}") or "{}"))

    moves, leftovers = plan_moves(
        [str(item.get("key") or "") for item in root_items], uploads, args.tag
    )
    for warning in stale_uploads([str(item.get("key") or "") for item in root_items], uploads):
        print(f"::warning::{warning}", file=sys.stderr)

    print(f"root now: {len(root_items)} objects, {len(uploads)} to upload, {len(moves)} to move")
    for source, target in moves:
        print(f"  history {source} -> {target}")
    if leftovers:
        print(f"  keep {len(leftovers)}: {', '.join(sorted(leftovers))}")

    digests = dict(previous)
    digests.update({upload["key"]: {"sha256": upload["sha256"], "size": upload["size"]} for upload in uploads})
    for source, _ in moves:
        digests.pop(source, None)

    if args.print_index:
        sys.stdout.write(render_index(root_items, domain, args.repo, digests))
        return 0

    if args.dry_run:
        after = [
            item for item in root_items if str(item.get("key")) not in {source for source, _ in moves}
        ]
        for upload in uploads:
            after.append({"key": upload["key"], "fsize": upload["size"], "putTime": 0})
        print("--- SHA256SUMS ---")
        sys.stdout.write(render_sums(after, digests))
        print("--- index.json ---")
        sys.stdout.write(render_index(after, domain, args.repo, digests))
        return 0

    for source, target in moves:
        kodo.move(source, target)
    for upload in uploads:
        kodo.upload(upload["path"], upload["key"])
        print(f"  uploaded {upload['key']}")

    with tempfile.TemporaryDirectory(prefix="qiniu-") as scratch:
        staged = Path(scratch)
        # Re-list instead of reasoning about the moves: the checksum list and
        # the index must describe the bucket, not this script's expectations.
        root_items = kodo.list_root()

        sums_text = render_sums(root_items, digests)
        sums_path = staged / SUMS_KEY
        sums_path.write_text(sums_text, encoding="utf-8", newline="\n")
        kodo.upload(sums_path, SUMS_KEY)
        kodo.upload(sums_path, f"{HISTORY_PREFIX}{args.tag}/{SUMS_KEY}")
        print(f"  uploaded {SUMS_KEY} ({len(sums_text.splitlines())} lines)")

        index = render_index(root_items, domain, args.repo, digests)
        index_path = staged / INDEX_KEY
        index_path.write_text(index, encoding="utf-8", newline="\n")
        kodo.upload(index_path, INDEX_KEY)

        # The edge cached the previous copy for a year; without this the list a
        # client reads would still describe the last release.
        for url in (f"{domain}/{INDEX_KEY}", f"{domain}/{SUMS_KEY}"):
            problem = kodo.refresh([url])
            if problem is None:
                print(f"  refreshed {url}")
            else:
                print(
                    f"warning: CDN refresh of {url} failed ({problem}); the edge may serve "
                    "the previous copy until it expires — purge it in the console",
                    file=sys.stderr,
                )

    document = json.loads(index)
    print(
        f"index: {len(document['packages'])} packages, "
        f"{len(document['tools'])} toolchains, {len(document['svcd'])} daemon archives"
    )
    print(f"done: {domain}/{INDEX_KEY}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
