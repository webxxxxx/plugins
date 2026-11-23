#!/usr/bin/env python3
"""打发布 tag：改版本号、提交、打 tag、推送。

用法::

    python tools/tag.py all patch            # 全量发布：plugin-tools 0.2.1 -> 0.2.2，tag v0.2.2
    python tools/tag.py plugin-store 0.1.18  # 单插件发布：tag plugin-store-v0.1.18
    python tools/tag.py sshd minor --dry-run # 只看会改什么，不落盘

两个 tag 形态对应 `.github/workflows/release.yml` 的两种发布：

- `all`  → `v<version>`，构建 `plugin-tools` + 全部插件，版本取自 `tools/cli/Cargo.toml`；
- 插件名 → `<plugin>-v<version>`，只构建该插件，版本取自 `plugins/<plugin>/package/manifest.json`。

脚本会把该同步的地方一次改完：Rust 插件的 `Cargo.toml` 和 `Cargo.lock` 一起改。
只改 `Cargo.toml` 不改 lock 时，CI 的 `cargo --locked` 会在第一步就报
"cannot update the lock file ... because --locked was passed"。

版本号里带 `-` 的部分（`0.1.18-rc1`）只进 tag，manifest 里写的是不带后缀的
`0.1.18`——release.yml 正是拿 `版本里第一个 -` 之前的部分去比 manifest。
"""

from __future__ import annotations

import json
import re
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

VERSION_RE = re.compile(r"^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$")
BUMP_RE = re.compile(r"^(major|minor|patch)$")


class Fail(Exception):
    pass


# --------------------------------------------------------------------------- #
# git


def git(*args: str, check: bool = True) -> str:
    proc = subprocess.run(
        ["git", *args],
        cwd=ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    if check and proc.returncode != 0:
        raise Fail(f"git {' '.join(args)} 失败：\n{proc.stdout.strip()}")
    return proc.stdout.strip()


def remote_repo() -> str:
    url = git("remote", "get-url", "origin")
    match = re.search(r"github\.com[:/](.+?)(?:\.git)?$", url)
    return match.group(1) if match else url


def tracked(path: Path) -> bool:
    """这个文件在版本库里吗？不进库的文件（比如插件本地生成的 Cargo.lock）只改不提交。"""
    proc = subprocess.run(
        ["git", "ls-files", "--error-unmatch", "--", path.relative_to(ROOT).as_posix()],
        cwd=ROOT,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    return proc.returncode == 0


# --------------------------------------------------------------------------- #
# 版本文件


def read_text(path: Path) -> tuple[str, str]:
    """返回（正文, 换行符）。工作区里的 CRLF 必须原样写回去，否则整文件都会变成 diff。"""
    text = path.read_bytes().decode("utf-8")
    return text, ("\r\n" if "\r\n" in text else "\n")


def cargo_version(path: Path) -> str:
    return cargo_version_text(read_text(path)[0], path)


def cargo_version_text(text: str, path: Path | None = None) -> str:
    """`[package]` 段里的 version，别碰 `[dependencies]` 里的版本约束。"""
    in_package = False
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith("["):
            in_package = stripped == "[package]"
            continue
        if in_package:
            match = re.fullmatch(r'version\s*=\s*"([^"]*)"', stripped)
            if match:
                return match.group(1)
    raise Fail(f"{path or 'Cargo.toml'} 的 [package] 里没有 version")


def set_cargo_version(path: Path, version: str) -> str:
    text, nl = read_text(path)
    lines = text.split(nl)
    in_package = False
    for index, line in enumerate(lines):
        stripped = line.strip()
        if stripped.startswith("["):
            in_package = stripped == "[package]"
            continue
        if in_package and re.fullmatch(r'version\s*=\s*"[^"]*"', stripped):
            old = cargo_version_text(text, path)
            lines[index] = re.sub(r'(version\s*=\s*)"[^"]*"', rf'\g<1>"{version}"', line)
            write_checked(path, nl.join(lines), lambda body: cargo_version_text(body) != version)
            return old
    raise Fail(f"{path} 的 [package] 里没有 version")


def lock_version(path: Path, package: str) -> str:
    return lock_version_text(read_text(path)[0], package, path)


def lock_version_text(text: str, package: str, path: Path | None = None) -> str:
    """读 `[[package]]` 段里某个包的 version。"""
    for name, version in _lock_entries(text):
        if name == package:
            return version
    raise Fail(f"{path or 'Cargo.lock'} 里没有 {package} 的 [[package]] 段")


def set_lock_version(path: Path, package: str, version: str) -> str:
    text, nl = read_text(path)
    lines = text.split(nl)
    in_block = False
    name = None
    for index, line in enumerate(lines + [""]):  # 末尾补一个空行，好收掉最后一个块
        stripped = line.strip()
        if stripped == "[[package]]":
            in_block, name = True, None
            continue
        if not in_block:
            continue
        if stripped == "":
            in_block = False
            continue
        if stripped.startswith("name = "):
            name = stripped.split("=", 1)[1].strip().strip('"')
            continue
        if stripped.startswith("version = ") and name == package:
            old = lock_version_text(text, package, path)
            lines[index] = re.sub(r'"([^"]*)"', f'"{version}"', line, count=1)
            body = nl.join(lines)
            write_checked(path, body, lambda candidate: lock_version_text(candidate, package) != version)
            return old
    raise Fail(f"{path} 里没有 {package} 的 [[package]] 段")


def _lock_entries(text: str) -> list[tuple[str, str]]:
    entries: list[tuple[str, str]] = []
    name = None
    version = None
    for line in text.splitlines() + [""]:
        stripped = line.strip()
        if stripped == "[[package]]" or stripped == "":
            if name and version:
                entries.append((name, version))
            name = version = None
            continue
        if stripped.startswith("name = ") and name is None:
            name = stripped.split("=", 1)[1].strip().strip('"')
        elif stripped.startswith("version = ") and version is None:
            version = stripped.split("=", 1)[1].strip().strip('"')
    return entries


def json_version(path: Path) -> str:
    return json.loads(read_text(path)[0]).get("version", "")


def set_json_version(path: Path, version: str) -> str:
    """只替换顶层那一行 `"version": "..."` 的值，其余字节不动。"""
    text, nl = read_text(path)
    lines = text.split(nl)
    candidates = []
    for index, line in enumerate(lines):
        match = re.fullmatch(r'(\s*)"version"\s*:\s*"([^"]*)"(,?)(\s*)', line)
        if match:
            candidates.append((len(match.group(1)), index, match.group(2), match))
    if not candidates:
        raise Fail(f"{path} 里没有顶层的 version")
    indent, index, old, match = min(candidates, key=lambda item: item[0])
    if len([c for c in candidates if c[0] == indent]) > 1:
        raise Fail(f"{path} 里有多个缩进相同的 version，不敢改")
    lines[index] = lines[index][: match.start(2)] + version + lines[index][match.end(2) :]
    body = nl.join(lines)
    if json.loads(body).get("version") != version:
        raise Fail(f"{path} 改完对不上，没写；请检查这份 manifest")
    path.write_bytes(body.encode("utf-8"))
    return old


def write_checked(path: Path, body: str, is_wrong) -> None:
    """先校验再落盘：改坏了就把文件留在原样，别留半截。"""
    if is_wrong(body):
        raise Fail(f"{path} 改完对不上，没写")
    path.write_bytes(body.encode("utf-8"))


# --------------------------------------------------------------------------- #
# 版本号


def core(version: str) -> tuple[int, int, int]:
    head = version.split("-", 1)[0]
    parts = head.split(".")
    if len(parts) != 3 or not all(p.isdigit() for p in parts):
        raise Fail(f"版本号看不懂：{version}")
    return int(parts[0]), int(parts[1]), int(parts[2])


def next_version(current: str, spec: str) -> str:
    if VERSION_RE.match(spec):
        return spec
    if not BUMP_RE.match(spec):
        raise Fail(f"给个具体版本（如 0.1.18）或 major/minor/patch，收到：{spec}")
    major, minor, patch = core(current)
    if spec == "patch":
        # 预发布升补丁 = 直接转正：0.1.18-rc1 -> 0.1.18
        if "-" in current:
            return f"{major}.{minor}.{patch}"
        return f"{major}.{minor}.{patch + 1}"
    if spec == "minor":
        return f"{major}.{minor + 1}.0"
    return f"{major + 1}.0.0"


# --------------------------------------------------------------------------- #
# 计划


@dataclass
class Target:
    name: str          # "all" 或插件名
    tag: str
    files: list[Path]  # 会改、会提交的版本文件
    crates: list[Path]  # 需要 `cargo metadata --locked` 验一遍的 crate 目录
    untracked: list[str]  # 改了但不提交的（本地生成的 lock）


def plan(target: str, spec: str) -> tuple[Target, str, str, list[tuple[Path, str, str]]]:
    if target == "all":
        crate = ROOT / "tools" / "cli"
        if not (crate / "Cargo.toml").is_file():
            raise Fail("tools/cli/Cargo.toml 不存在")
        current = cargo_version(crate / "Cargo.toml")
        version = next_version(current, spec)
        edits = [
            (crate / "Cargo.toml", "cargo", ""),
            (crate / "Cargo.lock", "lock", "plugin-tools"),
        ]
        name = "plugin-tools"
        tag = f"v{version}"
        crates = [crate]
    else:
        directory = ROOT / "plugins" / target
        if not (directory / ".plugin.json").is_file():
            raise Fail(f"没有 plugins/{target}/.plugin.json，插件名写错了？")
        manifest = directory / "package" / "manifest.json"
        if not manifest.is_file():
            raise Fail(f"plugins/{target}/package/manifest.json 不存在")
        current = json_version(manifest)
        version = next_version(current, spec)
        name = target
        tag = f"{target}-v{version}"
        crates = [directory] if (directory / "Cargo.toml").is_file() else []
        edits = [
            (manifest, "json", target),
            (directory / "Cargo.toml", "cargo", ""),
            (directory / "Cargo.lock", "lock", target),
        ]

    # manifest 里写的是不带 `-` 的部分，tag 里才有。
    base = version.split("-", 1)[0]
    if core(base) < core(current):
        raise Fail(f"{base} 比当前版本 {current} 还小")
    # 脚本插件只有 manifest，Rust 插件才有 Cargo.toml / Cargo.lock。
    # 有的插件不把 Cargo.lock 进版本库（CI 自己生成），本地存在就一起改，
    # 但不往提交里塞。
    edits = [entry for entry in edits if entry[0].is_file()]
    files = [entry[0] for entry in edits if tracked(entry[0])]
    untracked = [
        entry[0].relative_to(ROOT).as_posix()
        for entry in edits
        if entry[0] not in files
    ]
    if not files:
        raise Fail("没有可提交的版本文件，检查一下仓库状态")

    return (
        Target(name=name, tag=tag, files=files, crates=crates, untracked=untracked),
        version,
        base,
        edits,
    )


def read_edit(path: Path, kind: str, package: str) -> str:
    if kind == "cargo":
        return cargo_version(path)
    if kind == "lock":
        return lock_version(path, package)
    return json_version(path)


def write_edit(path: Path, kind: str, package: str, version: str) -> str:
    if kind == "cargo":
        return set_cargo_version(path, version)
    if kind == "lock":
        return set_lock_version(path, package, version)
    return set_json_version(path, version)


# --------------------------------------------------------------------------- #
# 主流程


def check_clean(branch: str) -> None:
    dirty = [line for line in git("status", "--porcelain", "--untracked-files=no").splitlines() if line]
    if dirty:
        raise Fail("工作区有未提交的改动，先 commit 或 stash：\n" + "\n".join(dirty))

    current = git("rev-parse", "--abbrev-ref", "HEAD")
    if current != branch:
        raise Fail(f"当前在 {current} 上，发布要从 {branch} 打")


def sync_with_remote(branch: str) -> None:
    git("fetch", "origin", branch)
    counts = git("rev-list", "--left-right", "--count", f"HEAD...origin/{branch}").split()
    ahead, behind = int(counts[0]), int(counts[1])
    if behind and ahead:
        raise Fail(
            f"本地和 origin/{branch} 分叉了（{ahead} 个没推、{behind} 个没拉），先 git pull --rebase"
        )
    if behind:
        print(f"  本地落后 origin/{branch} {behind} 个提交，先快进")
        git("merge", "--ff-only", f"origin/{branch}")
    if ahead:
        print(f"  注意：本地有 {ahead} 个未推送的提交，这次发布会一起带上去")


def check_tag_free(tag: str) -> None:
    if git("tag", "--list", tag):
        raise Fail(f"本地已经有 tag {tag}；换个版本号，或先 git tag -d {tag}")
    if git("ls-remote", "--tags", "origin", f"refs/tags/{tag}"):
        raise Fail(f"远端已经有 tag {tag}；换个版本号（已经发过的版本不能再发）")


def verify_crates(crates: list[Path]) -> None:
    # 进版本库的 Cargo.lock 要按 CI 的 `--locked` 验；插件自己的 lock 一般不进库
    # （CI 用 build.sh 构建，不带 --locked），这里只证明依赖能被解析出来。
    for crate in crates:
        locked = tracked(crate / "Cargo.lock")
        label = "cargo metadata --locked --offline" if locked else "cargo metadata --offline"
        print(f"  {label}：{crate.relative_to(ROOT).as_posix()}")
        command = ["cargo", "metadata", "--offline", "--format-version", "1"]
        if locked:
            command.insert(2, "--locked")
        proc = subprocess.run(
            command,
            cwd=crate,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
            errors="replace",
        )
        if proc.returncode != 0:
            hint = "（CI 会在同一步失败）" if locked else ""
            raise Fail(
                f"{crate.relative_to(ROOT).as_posix()} 过不了 {label} 校验{hint}：\n" + proc.stderr.strip()
            )


def commit(target: Target, version: str, branch: str, push: bool) -> None:
    paths = [path.relative_to(ROOT).as_posix() for path in target.files]
    git("add", "--", *paths)
    staged = git("diff", "--cached", "--name-only")
    if not staged:
        print("  版本号没变，跳过一次空提交")
    else:
        git("commit", "-m", f"chore(release): {target.name} v{version}")
        print(f"  提交：{git('log', '-1', '--oneline')}")
    if not push:
        return

    git("fetch", "origin", branch)
    if int(git("rev-list", "--count", f"HEAD..origin/{branch}")):
        print(f"  origin/{branch} 又动了，rebase 一次再推")
        git("rebase", f"origin/{branch}")

    try:
        git("push", "origin", f"HEAD:{branch}")
    except Fail:
        git("fetch", "origin", branch)
        git("rebase", f"origin/{branch}")
        git("push", "origin", f"HEAD:{branch}")
    print(f"  已推送 {branch}")


def tag(target: Target, push: bool) -> None:
    if git("tag", "--list", target.tag):
        raise Fail(f"本地已经有 tag {target.tag}")
    git("tag", target.tag)
    if not push:
        print(f"  已在本地打 tag {target.tag}")
        return
    try:
        git("push", "origin", f"refs/tags/{target.tag}")
    except Fail:
        raise Fail(f"tag 建好了但推不上去；手动重试：git push origin {target.tag}")
    print(f"  已推送 tag {target.tag}")


def main(argv: list[str]) -> int:
    args = [arg for arg in argv if not arg.startswith("-")]
    flags = {arg for arg in argv if arg.startswith("-")}
    dry_run = bool(flags & {"-n", "--dry-run"})
    no_push = bool(flags & {"--no-push"})
    no_verify = "--no-verify" in flags
    branch = "main"
    for arg in argv:
        if arg.startswith("--branch="):
            branch = arg.split("=", 1)[1]

    if len(args) != 2 or flags & {"-h", "--help"}:
        print(__doc__.split("两个 tag 形态")[0].strip())
        print("\n参数：[all|<插件名>] [major|minor|patch|<x.y.z>] [--dry-run] [--no-push] [--no-verify] [--branch=main]")
        return 0 if flags & {"-h", "--help"} else 2

    target_name, spec = args
    target, _version, base, edits = plan(target_name, spec)

    # 版本号里的 -rc1 只进 tag，写进文件的是 base。
    current = read_edit(*edits[0])
    print(f"目标：{target.name}  {current} -> {base}  tag {target.tag}")
    if target.untracked:
        print(f"  （只改不提交，本地生成的文件：{', '.join(target.untracked)}）")
    for path, kind, package in edits:
        print(f"  {path.relative_to(ROOT).as_posix()}：{read_edit(path, kind, package)} -> {base}")

    if dry_run:
        print("\n--dry-run：没有写文件、没有提交、没有打 tag")
        return 0

    check_clean(branch)
    sync_with_remote(branch)
    check_tag_free(target.tag)

    for path, kind, package in edits:
        write_edit(path, kind, package, base)
    if not no_verify:
        verify_crates(target.crates)

    if no_push:
        print("\n--no-push：只在本地产提交和 tag，不推送")
    commit(target, base, branch, push=not no_push)
    tag(target, push=not no_push)

    repo = remote_repo()
    print(f"\n完成：{target.tag}")
    print(f"  看进展：gh run list --repo {repo} --workflow=release.yml --limit 1")
    print("  发完 CI 会自己提交一份 docs/plugins.md，记得回头 git pull")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except Fail as error:
        print(f"错误：{error}", file=sys.stderr)
        sys.exit(1)
