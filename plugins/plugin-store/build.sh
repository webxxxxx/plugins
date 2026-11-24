#!/bin/sh
# 编 plugin-store：宿主上直接编，SVCD_DOCKER=1 时整段丢给 tools/docker 的 rust-cross 镜像。
#
# 为什么交叉编这么麻烦：ureq 走 rustls，rustls 用 ring，ring 里有 C 和一小段汇编。
# 光 `rustup target add` 只解决 Rust 那一半，C 那一半仍然用宿主的 cc，编出 x86_64 的
# 目标文件，链接到 arm 二进制时才炸：
#   /usr/bin/ld: .../crt1.o: relocations in generic ELF (EM: 40)
#   /usr/bin/ld: .../crt1.o: error adding symbols: file in wrong format
# 所以下面给每个 musl 目标配好 CC_/AR_/CARGO_TARGET_*_LINKER；宿主机工具链不全就
# 设 SVCD_DOCKER=1 交给容器（镜像里 clang、musl-tools、/usr/soft 交叉工具链都有）。
set -eu
NAME=$1
VERSION=$2
PLATFORM=$3
HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
command -v cargo >/dev/null 2>&1 || PATH="$HOME/.cargo/bin:$PATH"

die() { echo "plugin-store: $*" >&2; exit 1; }

# ---- 在容器里编：宿主机只要 docker ----
if [ "${SVCD_DOCKER:-0}" = 1 ] && [ "${SVCD_IN_DOCKER:-0}" != 1 ]; then
    REPO=$(CDPATH= cd -- "$HERE/../.." && pwd)
    [ -f "$REPO/tools/docker/docker-build.sh" ] || die "SVCD_DOCKER=1 但找不到 tools/docker/docker-build.sh"
    echo "plugin-store: docker 里编 $PLATFORM" >&2
    # 项目根钉成仓库根，容器里的 cwd 就是 /work，所以脚本用仓库相对路径
    exec sh "$REPO/tools/docker/docker-build.sh" -r "$REPO" -p "$PLATFORM" \
        -w . -n plugin-store -- \
        sh plugins/plugin-store/build.sh "$NAME" "$VERSION" "$PLATFORM"
fi

# 交叉工具链的根目录：$1 是编译器名，其余是候选目录；打印第一个真的装着它的。
cross_root() {
    _compiler=$1
    shift
    for _dir in "$@"; do
        [ -n "$_dir" ] || continue
        if [ -x "$_dir/bin/$_compiler" ]; then
            printf '%s' "$_dir"
            return 0
        fi
    done
    return 1
}

# 给 cargo 的 env 前缀（值里没有空格，展开就行）
EV=

cross_setup() {
    _triple=$1
    _upper=$(printf '%s' "$_triple" | tr 'a-z-.' 'A-Z__')
    _root=
    _cc=
    case "$_triple" in
    armv7-unknown-linux-musleabihf)
        _root=$(cross_root arm-linux-musleabihf-gcc "${CROSS_PREFIX:-}" /usr/soft/arm-linux-musleabihf-cross) ||
            die "找不到 arm-linux-musleabihf-gcc：装到 /usr/soft 或设 CROSS_PREFIX；也可以 SVCD_DOCKER=1 交给容器"
        _cc=arm-linux-musleabihf-gcc
        ;;
    aarch64-unknown-linux-musl)
        _root=$(cross_root aarch64-linux-musl-gcc "${AARCH64_CROSS:-}" /usr/soft/aarch64-linux-musl-cross) ||
            die "找不到 aarch64-linux-musl-gcc：装到 /usr/soft 或设 AARCH64_CROSS；也可以 SVCD_DOCKER=1 交给容器"
        _cc=aarch64-linux-musl-gcc
        ;;
    x86_64-unknown-linux-musl)
        if command -v musl-gcc >/dev/null 2>&1; then
            # debian 的 musl-tools：只有 musl-gcc，没有 musl-g++
            EV="AR_$_upper=ar"
            _cc=musl-gcc
        else
            _root=$(cross_root x86_64-linux-musl-gcc "${X86_64_MUSL_CROSS:-${CROSS_PREFIX:-}}" /usr/soft/x86_64-linux-musl-cross) ||
                die "找不到 musl-gcc 或 x86_64-linux-musl-gcc；宿主机装 musl-tools，或 SVCD_DOCKER=1"
            _cc=x86_64-linux-musl-gcc
        fi
        ;;
    *) return 0 ;; # 原生目标，什么都不用配
    esac
    if [ -n "$_root" ]; then
        export PATH="$_root/bin:$PATH"
        _prefix=${_cc%-gcc}
        EV="AR_$_upper=$_prefix-ar CXX_$_upper=$_prefix-g++ $EV"
    fi
    EV="CC_$_upper=$_cc CARGO_TARGET_${_upper}_LINKER=$_cc $EV"
}

cross_setup "$PLATFORM"

# 目标没装时 cargo 只会说 "can't find crate for core"
if command -v rustup >/dev/null 2>&1 && ! rustup target list --installed 2>/dev/null | grep -qx "$PLATFORM"; then
    die "当前工具链没有 $PLATFORM 目标：rustup target add $PLATFORM"
fi

cd "$HERE"
# /mnt/d 上直接编会跟 drvfs 较劲；容器里的 CARGO_TARGET_DIR 由 docker-build.sh 指到
# /usr/soft/build_workspace，宿主上就用仓库里的 target/。
TARGET_DIR=${CARGO_TARGET_DIR:-$HERE/target}

# shellcheck disable=SC2086 # $EV 是刻意留白的 env 赋值串
env $EV cargo build --release --target "$PLATFORM"

mkdir -p "$HERE/out"
case "$PLATFORM" in
*windows*) cp "$TARGET_DIR/$PLATFORM/release/$NAME.exe" "$HERE/out/$NAME.exe" ;;
*) cp "$TARGET_DIR/$PLATFORM/release/$NAME" "$HERE/out/$NAME" ;;
esac
echo "plugin-store: $HERE/out/$NAME" >&2
