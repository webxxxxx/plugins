#!/bin/sh
# 本机打包插件：编译在 docker 镜像里做，产物统一拷到仓库的 dist/。
#
#   sh tools/pack.sh                                # meow：windows-msvc + linux-gnu
#   sh tools/pack.sh meow x86_64-unknown-linux-musl # 换/加平台
#   sh tools/pack.sh sshd                           # 别的插件（它的 build.sh 支持 SVCD_DOCKER 就行）
#
# 宿主只需要：docker（当前用户在 docker 组）+ 宿主的 `plugin` 二进制（bundle 用）。
# plugin-tools（打包器）缺失时会在容器里现编，编好的副本放 bin/。
set -eu

REPO=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
OUT_DIR=${OUT_DIR:-$REPO/dist}
WORKSPACE=${SVCD_BUILD_WORKSPACE:-/usr/soft/build_workspace}
IMAGE=${SVCD_CROSS_IMAGE:-rust-cross:1.98.1}

die() { echo "pack: $*" >&2; exit 1; }

usage() {
    cat >&2 <<'USAGE'
用法：sh tools/pack.sh [插件] [平台...]

  不带参数            meow 的 windows-msvc + linux-gnu
  <插件>              插件目录名（plugins/ 下面那个），默认 meow
  <三元组>            想打哪个平台就写哪个，可以写多个
  -p, --platform <t>  同上，显式写法
  OUT_DIR=<dir>       产物落哪（默认 <仓库>/dist）

例子：
  sh tools/pack.sh
  sh tools/pack.sh meow x86_64-unknown-linux-musl
  sh tools/pack.sh -p x86_64-pc-windows-msvc -p x86_64-unknown-linux-gnu
  sh tools/pack.sh sshd x86_64-unknown-linux-musl
USAGE
}

NAME=meow
PLATFORMS=
while [ $# -gt 0 ]; do
    case "$1" in
    -h|--help) usage; exit 0 ;;
    -p|--platform) [ $# -ge 2 ] || die "-p 后面要跟平台"; PLATFORMS="$PLATFORMS $2"; shift 2 ;;
    --platform=*) PLATFORMS="$PLATFORMS ${1#--platform=}"; shift ;;
    -*) die "未知选项 $1（--help 看用法）" ;;
    *-*-*) PLATFORMS="$PLATFORMS $1"; shift ;;   # 三元组长这样
    *) NAME=$1; shift ;;
    esac
done
[ -n "$PLATFORMS" ] || PLATFORMS="x86_64-pc-windows-msvc x86_64-unknown-linux-gnu"

[ -f "$REPO/plugins/$NAME/package/manifest.json" ] || die "没有这个插件：plugins/$NAME"

# ---- docker 与镜像 ----
command -v docker >/dev/null 2>&1 || die "找不到 docker"
docker info >/dev/null 2>&1 || die "连不上 docker daemon（当前用户在 docker 组里吗？重开一个 WSL 会话）"
if ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
    echo "pack: 构建镜像 $IMAGE（第一次几分钟）…" >&2
    docker build -f "$REPO/tools/docker/rust-cross.Dockerfile" -t "$IMAGE" "$REPO" ||
        die "镜像构建失败"
fi

# ---- plugin-tools：优先用已有的副本，没有就在容器里编一个 ----
TOOLS=${PLUGIN_TOOLS:-$REPO/bin/plugin-tools}
if [ ! -x "$TOOLS" ]; then
    echo "pack: 在容器里编 plugin-tools…" >&2
    sh "$REPO/tools/docker/run.sh" -w tools/cli -n plugin-tools -- cargo build --release --locked ||
        die "plugin-tools 编译失败"
    mkdir -p "$REPO/bin"
    cp "$WORKSPACE/plugin-tools/target/release/plugin-tools" "$REPO/bin/plugin-tools"
    TOOLS=$REPO/bin/plugin-tools
fi

# ---- 宿主的 plugin（bundle 用） ----
if [ -z "${SVCD_PLUGIN_BIN:-}" ]; then
    for candidate in "$REPO/bin/plugin" "$REPO/../meow-dist/host-linux/plugin" "$(command -v plugin 2>/dev/null || true)"; do
        [ -n "$candidate" ] && [ -x "$candidate" ] && SVCD_PLUGIN_BIN=$candidate && break
    done
    [ -n "${SVCD_PLUGIN_BIN:-}" ] ||
        die "找不到宿主的 plugin 二进制：用 SVCD_PLUGIN_BIN=/path/to/plugin 指一个"
    export SVCD_PLUGIN_BIN
fi

# ---- 逐平台打包 ----
mkdir -p "$OUT_DIR"
for platform in $PLATFORMS; do
    echo "pack: $NAME $platform" >&2
    SVCD_DOCKER=1 "$TOOLS" build --dir "$REPO/plugins/$NAME" --platform "$platform" ||
        die "$NAME $platform 打包失败"
    # 按清单里的版本取那一个：dist/ 里可能躺着以前版本的包，别拷错了。
    version=$(sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' \
        "$REPO/plugins/$NAME/package/manifest.json" | head -n 1)
    produced=$REPO/plugins/$NAME/dist/$NAME-$version-$platform.spk
    [ -f "$produced" ] || die "$NAME $platform 没产出 $produced"
    cp "$produced" "$OUT_DIR/"
    echo "pack: → dist/$(basename "$produced")" >&2
done

echo
ls -l "$OUT_DIR"/*.spk
command -v sha256sum >/dev/null 2>&1 && sha256sum "$OUT_DIR"/*.spk
