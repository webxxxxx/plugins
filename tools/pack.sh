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
  -p, --platform <t>  同上，显式写法；也可以写 <三元组>:<device> 打设备变体
  -d, --device <id>   设备变体（如 sr6110），只能配单个平台用
  all                 这个插件 .plugin.json 里除 mac 之外的全部，含 variants（本地能编的都编一遍）
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
DEVICE=
want_some=   # all / linux / musl：按 .plugin.json 展开成三元组（plugin-tools units 算）
while [ $# -gt 0 ]; do
    case "$1" in
    -h|--help) usage; exit 0 ;;
    -p|--platform) [ $# -ge 2 ] || die "-p 后面要跟平台"; PLATFORMS="$PLATFORMS $2"; shift 2 ;;
    --platform=*) PLATFORMS="$PLATFORMS ${1#--platform=}"; shift ;;
    -d|--device) [ $# -ge 2 ] || die "-d 后面要跟设备 id"; DEVICE=$2; shift 2 ;;
    --device=*) DEVICE=${1#--device=}; shift ;;
    -*) die "未知选项 $1（--help 看用法）" ;;
    all|every) want_some=all; shift ;;
    linux) want_some=linux; shift ;;
    musl) want_some=musl; shift ;;
    *-*-*) PLATFORMS="$PLATFORMS $1"; shift ;;   # 三元组长这样
    *) NAME=$1; shift ;;
    esac
done

[ -f "$REPO/plugins/$NAME/package/manifest.json" ] || die "没有这个插件：plugins/$NAME"

[ -n "$(printf '%s' "$PLATFORMS" | tr -d ' ')" ] || PLATFORMS="x86_64-pc-windows-msvc x86_64-unknown-linux-gnu"

# -d 只能配单个平台：变体本来就只属于某一个三元组。
if [ -n "$DEVICE" ]; then
    count=$(printf '%s' "$PLATFORMS" | tr ' ' '\n' | grep -c . || true)
    [ "$count" = 1 ] || die "--device 要和单个平台一起用（现在有 $count 个）"
    PLATFORMS="$(printf '%s' "$PLATFORMS" | tr -d ' '):$DEVICE"
fi

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

# ---- 单元清单：只有一个来源，plugin-tools units ----
# 「哪些包」由插件的 .plugin.json 决定（ci.platforms / platforms + variants），产物名也
# 由它拼——以前这段在这里用内联 python 摊一遍、再用 sed 从清单里抠版本手拼产物名，
# 和 `plugin-tools build` 各有一份实现。这里只留「这台机器编得了哪些」的本地知识：
# all/linux/musl 的过滤与 darwin 跳过。
units_of() {
    "$TOOLS" units --dir "$REPO/plugins/$NAME" "$@"
}

if [ -n "$want_some" ]; then
    entries=$(units_of | awk -F '\t' -v want="$want_some" '
        {
            if (want == "all") {
                if ($1 ~ /apple-darwin/) {
                    print "pack: 跳过 " $1 "（darwin 要有 Xcode，只有 mac 上能编）" > "/dev/stderr"
                    next
                }
            } else if (want == "linux") {
                if ($1 !~ /-linux-/) next
            } else if (want == "musl") {
                if ($1 !~ /-musl/) next
            }
            print $1 ":" $2 ":" $3
        }') || die "读不到 $NAME 的单元清单"
    [ -n "$entries" ] || die "$NAME 展开后没有可打的单元"
else
    entries=
    for spec in $PLATFORMS; do
        platform=${spec%%:*}
        device=
        [ "$platform" = "$spec" ] || device=${spec#*:}
        if [ -n "$device" ]; then
            line=$(units_of --platform "$platform" --device "$device") ||
                die "读不到 $NAME $platform ($device) 的单元"
        else
            line=$(units_of --platform "$platform") || die "读不到 $NAME $platform 的单元"
        fi
        [ -n "$line" ] || die "$NAME $platform 没有产物名"
        entries="$entries
$(printf '%s\n' "$line" | awk -F '\t' '{ print $1 ":" $2 ":" $3; exit }')"
    done
fi

# ---- 逐单元打包 ----
mkdir -p "$OUT_DIR"
for entry in $entries; do
    platform=${entry%%:*}
    rest=${entry#*:}
    device=${rest%%:*}
    artifact=${rest#*:}
    label=$platform
    [ -n "$device" ] && label="$platform ($device)"
    echo "pack: $NAME $label" >&2
    # 每个平台自己的镜像：gnu 用 bookworm 老底子，其余用默认（见 tools/docker/image.sh）
    platform_image=$(sh "$REPO/tools/docker/image.sh" "$platform") || die "镜像准备失败"
    set -- --dir "$REPO/plugins/$NAME" --platform "$platform"
    [ -n "$device" ] && set -- "$@" --device "$device"
    SVCD_CROSS_IMAGE=$platform_image SVCD_DOCKER=1 "$TOOLS" build "$@" ||
        die "$NAME $label 打包失败"
    # 产物名来自 units：dist/ 里可能躺着以前版本的包，按名字取就不会拷错。
    produced=$REPO/plugins/$NAME/dist/$artifact
    [ -f "$produced" ] || die "$NAME $label 没产出 $produced"
    cp "$produced" "$OUT_DIR/"
    echo "pack: → dist/$(basename "$produced")" >&2
done

echo
ls -l "$OUT_DIR"/*.spk
command -v sha256sum >/dev/null 2>&1 && sha256sum "$OUT_DIR"/*.spk
