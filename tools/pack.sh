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

die() { echo "pack: $*" >&2; exit 1; }

usage() {
    cat >&2 <<'USAGE'
用法：sh tools/pack.sh [插件] [平台...]

  不带参数            meow 的 windows-msvc + linux-gnu
  <插件>              插件目录名（plugins/ 下面那个），默认 meow
  <三元组>            想打哪个平台就写哪个，可以写多个
  -p, --platform <t>  同上，显式写法
  all                 这个插件 .plugin.json 里除 mac 之外的全部（本地能编的都编一遍）
  linux               只要 linux 那几个（gnu + 三个 musl）
  musl                只要 musl 那几个
  OUT_DIR=<dir>       产物落哪（默认 <仓库>/dist）

  除了 mac，镜像 + 交叉工具链能把 5 个目标全打出来（gnu、x86_64/aarch64/armv7 musl、
  windows-msvc）；mac 两个目标要 Xcode，只能在 mac 上编，所以 `all` 会把它们跳掉。

例子：
  sh tools/pack.sh                         # windows-msvc + linux-gnu
  sh tools/pack.sh meow all                # 除 mac 外全打（测试用）
  sh tools/pack.sh meow linux              # 只打 linux 的四个
  sh tools/pack.sh meow x86_64-unknown-linux-musl
  sh tools/pack.sh -p x86_64-pc-windows-msvc -p x86_64-unknown-linux-gnu
  sh tools/pack.sh sshd x86_64-unknown-linux-musl
USAGE
}

NAME=meow
PLATFORMS=
want_some=   # all / linux / musl：按 .plugin.json 展开成三元组
while [ $# -gt 0 ]; do
    case "$1" in
    -h|--help) usage; exit 0 ;;
    -p|--platform) [ $# -ge 2 ] || die "-p 后面要跟平台"; PLATFORMS="$PLATFORMS $2"; shift 2 ;;
    --platform=*) PLATFORMS="$PLATFORMS ${1#--platform=}"; shift ;;
    -*) die "未知选项 $1（--help 看用法）" ;;
    all|every) want_some=all; shift ;;
    linux) want_some=linux; shift ;;
    musl) want_some=musl; shift ;;
    *-*-*) PLATFORMS="$PLATFORMS $1"; shift ;;   # 三元组长这样
    *) NAME=$1; shift ;;
    esac
done

[ -f "$REPO/plugins/$NAME/package/manifest.json" ] || die "没有这个插件：plugins/$NAME"

# ---- all / linux / musl：平台的唯一来源是插件的 .plugin.json ----
if [ -n "$want_some" ]; then
    [ -f "$REPO/plugins/$NAME/.plugin.json" ] ||
        die "plugins/$NAME 没有 .plugin.json，$want_some 展开不了；直接把三元组列出来"
    list=$(
        sed -n '/"platforms"[[:space:]]*:/,/]/p' "$REPO/plugins/$NAME/.plugin.json" |
            tr -d ' \t' | tr ',' '\n' | sed -n 's/^"\([^"]*\)".*/\1/p' | grep -- '-'
    )
    [ -n "$list" ] || die "plugins/$NAME/.plugin.json 里没读到平台清单（all/linux/musl 靠它展开）"
    for platform in $list; do
        case "$want_some:$platform" in
        all:*-apple-darwin*)
            echo "pack: 跳过 $platform（darwin 要有 Xcode，只有 mac 上能编）" >&2
            continue
            ;;
        linux:*-linux-*) PLATFORMS="$PLATFORMS $platform" ;;
        musl:*-musl*) PLATFORMS="$PLATFORMS $platform" ;;
        all:*) PLATFORMS="$PLATFORMS $platform" ;;
        esac
    done
fi
[ -n "$(printf '%s' "$PLATFORMS" | tr -d ' ')" ] || PLATFORMS="x86_64-pc-windows-msvc x86_64-unknown-linux-gnu"

# ---- docker 与镜像 ----
command -v docker >/dev/null 2>&1 || die "找不到 docker"
docker info >/dev/null 2>&1 || die "连不上 docker daemon（当前用户在 docker 组里吗？重开一个 WSL 会话）"
# 镜像按平台挑，缺了现建：x86_64-unknown-linux-gnu 用 bookworm（glibc 2.36，老发行版能跑），
# 其余用 trixie。plugin-tools 是纯 Rust，用默认那个就够。
IMAGE=$(sh "$REPO/tools/docker/image.sh") || die "镜像准备失败"
export SVCD_CROSS_IMAGE=$IMAGE

# ---- plugin-tools：优先用已有的副本，没有就在容器里编一个 ----
TOOLS=${PLUGIN_TOOLS:-$REPO/bin/plugin-tools}
if [ ! -x "$TOOLS" ]; then
    echo "pack: 在容器里编 plugin-tools…" >&2
    sh "$REPO/tools/docker/docker-build.sh" -r "$REPO" -w tools/cli -n plugin-tools -- cargo build --release --locked ||
        die "plugin-tools 编译失败"
    mkdir -p "$REPO/bin"
    cp "$WORKSPACE/plugin-tools/target/release/plugin-tools" "$REPO/bin/plugin-tools"
    TOOLS=$REPO/bin/plugin-tools
fi

# ---- 宿主的 plugin（bundle 用） ----
if [ -z "${SVCD_PLUGIN_BIN:-}" ]; then
    for candidate in "$REPO/bin/plugin" "$REPO/bin/plugin.exe" "$(command -v plugin 2>/dev/null || true)"; do
        if [ -n "$candidate" ] && [ -x "$candidate" ]; then
            SVCD_PLUGIN_BIN=$candidate
            break
        fi
    done
    if [ -z "${SVCD_PLUGIN_BIN:-}" ]; then
        die "找不到宿主的 plugin 二进制。编一个放进 bin/：
      在 plugin-manager 源码里 cargo build --release --bin svcd
      然后 cp target/release/svcd $REPO/bin/plugin
    或者 SVCD_PLUGIN_BIN=/path/to/plugin 指一个"
    fi
    export SVCD_PLUGIN_BIN
fi

# ---- 逐平台打包 ----
mkdir -p "$OUT_DIR"
for platform in $PLATFORMS; do
    echo "pack: $NAME $platform" >&2
    # 每个平台自己的镜像：gnu 用 bookworm 老底子，其余用默认（见 tools/docker/image.sh）
    platform_image=$(sh "$REPO/tools/docker/image.sh" "$platform") || die "镜像准备失败"
    SVCD_CROSS_IMAGE=$platform_image SVCD_DOCKER=1 "$TOOLS" build --dir "$REPO/plugins/$NAME" --platform "$platform" ||
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
