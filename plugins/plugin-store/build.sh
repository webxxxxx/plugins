#!/bin/sh
# 编 plugin-store：宿主上直接编，SVCD_DOCKER=1 时整段丢给 tools/docker 的 rust-cross 镜像。
#
# 为什么交叉编这么麻烦：ureq 走 rustls，rustls 用 ring，ring 里有 C 和一小段汇编。
# 光 `rustup target add` 只解决 Rust 那一半，C 那一半仍然用宿主的 cc，编出 x86_64 的
# 目标文件，链接到 arm 二进制时才炸：
#   /usr/bin/ld: .../crt1.o: relocations in generic ELF (EM: 40)
#   /usr/bin/ld: .../crt1.o: error adding symbols: file in wrong format
# 交叉工具链的映射在 tools/cross-env.sh（三个插件 + docker 入口共用一份），宿主机工具链
# 不全就设 SVCD_DOCKER=1 交给容器（镜像里 clang、musl-tools、/usr/soft 交叉工具链都有）。
set -eu
NAME=$1
VERSION=$2
PLATFORM=$3
HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPO=$(CDPATH= cd -- "$HERE/../.." && pwd)
command -v cargo >/dev/null 2>&1 || PATH="$HOME/.cargo/bin:$PATH"

die() { echo "plugin-store: $*" >&2; exit 1; }

# ---- 在容器里编：宿主机只要 docker ----
if [ "${SVCD_DOCKER:-0}" = 1 ] && [ "${SVCD_IN_DOCKER:-0}" != 1 ]; then
    [ -f "$REPO/tools/docker/docker-build.sh" ] || die "SVCD_DOCKER=1 但找不到 tools/docker/docker-build.sh"
    echo "plugin-store: docker 里编 $PLATFORM" >&2
    # 项目根钉成仓库根，容器里的 cwd 就是 /work，所以脚本用仓库相对路径
    exec sh "$REPO/tools/docker/docker-build.sh" -r "$REPO" -p "$PLATFORM" \
        -w . -n plugin-store -- \
        sh plugins/plugin-store/build.sh "$NAME" "$VERSION" "$PLATFORM"
fi

# 交叉工具链的映射只有一份：tools/cross-env.sh 按目标导出 PATH、CC_/CXX_/AR_ 和
# CARGO_TARGET_*_LINKER。认不出来（原生目标）就什么都不做。
. "$REPO/tools/cross-env.sh"
svcd_cross_apply "$PLATFORM" ||
    die "$SVCD_CROSS_ERROR；宿主机工具链不全就设 SVCD_DOCKER=1 交给容器"

# 目标没装时 cargo 只会说 "can't find crate for core"
if command -v rustup >/dev/null 2>&1 && ! rustup target list --installed 2>/dev/null | grep -qx "$PLATFORM"; then
    die "当前工具链没有 $PLATFORM 目标：rustup target add $PLATFORM"
fi

cd "$HERE"
# /mnt/d 上直接编会跟 drvfs 较劲；容器里的 CARGO_TARGET_DIR 由 docker-build.sh 指到
# /usr/soft/build_workspace，宿主上就用仓库里的 target/。
TARGET_DIR=${CARGO_TARGET_DIR:-$HERE/target}

cargo build --release --target "$PLATFORM"

mkdir -p "$HERE/out"
case "$PLATFORM" in
*windows*) cp "$TARGET_DIR/$PLATFORM/release/$NAME.exe" "$HERE/out/$NAME.exe" ;;
*) cp "$TARGET_DIR/$PLATFORM/release/$NAME" "$HERE/out/$NAME" ;;
esac
echo "plugin-store: $HERE/out/$NAME" >&2
