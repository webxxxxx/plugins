#!/bin/sh
# 打包前编译 payload 里要用的二进制。
#
#   DEVICE 为空      -> 通用包：gotatun（用户态）+ wg，两个都从 vendor/ 源码编
#   DEVICE=sr6110    -> SR6110 变体：内核模块 wireguard.ko 是唯一预编译件
#                       （要对着厂商 5.4.196 内核树编，CI 里没有那棵树），
#                       放在 vendor/prebuilt/；wg 仍然从源码编
#
# 交叉工具链走仓库里那张唯一的表 tools/cross-env.sh（别在这里另拼 CC/AR）。
# 有 docker 的机器可以 SVCD_DOCKER=1，把编译丢进 rust-cross 镜像，宿主什么都不用装。
set -eu
NAME=${1:-wireguard}
VERSION=${2:-0.0.0}
PLATFORM=${3:?platform is required}
DEVICE=${4:-${SVCD_DEVICE:-}}
HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPO=$(CDPATH= cd -- "$HERE/../.." && pwd)
OUT="$HERE/out/$PLATFORM"
VENDOR="$HERE/vendor"

die() { echo "wireguard: $*" >&2; exit 1; }

# 在容器里编：宿主只要 docker（WSL/Linux 上的 docker-build.sh）。
if [ "${SVCD_DOCKER:-0}" = 1 ] && [ "${SVCD_IN_DOCKER:-0}" != 1 ]; then
    [ -f "$REPO/tools/docker/docker-build.sh" ] || die "SVCD_DOCKER=1 但找不到 tools/docker/docker-build.sh"
    echo "wireguard: docker 里编 $PLATFORM（device=${DEVICE:-none}）" >&2
    exec sh "$REPO/tools/docker/docker-build.sh" -r "$REPO" -w . -n wireguard -- \
        sh plugins/wireguard/build.sh "$NAME" "$VERSION" "$PLATFORM" "$DEVICE"
fi

# payload 里的脚本必须是 LF：CRLF 会让设备上的 `set -u` 报 “illegal option”。
check_lf() {
    for f in "$HERE/package/payload/bin"/*.sh; do
        [ -f "$f" ] || continue
        if grep -q "$(printf '\r')" "$f" 2>/dev/null; then
            die "$f 里有 CRLF；payload 脚本必须用 LF"
        fi
    done
}
check_lf

case "$PLATFORM" in
x86_64-unknown-linux-musl | aarch64-unknown-linux-musl | \
    armv7-unknown-linux-musleabihf | x86_64-unknown-linux-gnu) ;;
*) die "不支持的平台 $PLATFORM" ;;
esac

case "$DEVICE" in
"") BACKEND=userspace ;;
sr6110) BACKEND=kernel ;;
*) die "未知的设备变体 $DEVICE（只支持 sr6110）" ;;
esac
if [ "$BACKEND" = kernel ] && [ "$PLATFORM" != aarch64-unknown-linux-musl ]; then
    die "sr6110 变体只对 aarch64-unknown-linux-musl 有意义（当前 $PLATFORM）"
fi

need() { command -v "$1" >/dev/null 2>&1 || die "缺少 $1（$2）"; }

# 三元组 → 工具链：唯一来源是 tools/cross-env.sh（它也会导出 CARGO_TARGET_*_LINKER）。
. "$REPO/tools/cross-env.sh"
svcd_cross_apply "$PLATFORM" || die "cross-env: $SVCD_CROSS_ERROR"
target_cc=${SVCD_CROSS_CC:-cc}

build_wg() {
    need make "构建 wg"
    need "$target_cc" "目标 C 工具链"

    build="$HERE/out/build/$PLATFORM/wireguard-tools"
    rm -rf "$build"
    mkdir -p "$(dirname "$build")"
    cp -R "$VENDOR/wireguard-tools" "$build"
    make -C "$build/src" clean >/dev/null 2>&1 || true
    make -C "$build/src" CC="$target_cc" LDFLAGS=-static PLATFORM=linux >/dev/null ||
        die "wg 编译失败（CC=$target_cc）"
    [ -x "$build/src/wg" ] || die "wg 编出来不在 $build/src/wg"
    cp -f "$build/src/wg" "$OUT/wg-tool"
    chmod 755 "$OUT/wg-tool"
}

build_gotatun() {
    need cargo "构建 gotatun"
    # --locked：Cargo.lock 进仓库，不许解析出新版本；--target-dir 放到 out/ 里，
    # vendor/ 保持原始源码。
    cargo build --locked --release --bin gotatun --target "$PLATFORM" \
        --manifest-path "$VENDOR/gotatun-0.9.2/Cargo.toml" \
        --target-dir "$HERE/out/cargo" >/dev/null ||
        die "gotatun 编译失败（target $PLATFORM，CC=$target_cc）"
    cp -f "$HERE/out/cargo/$PLATFORM/release/gotatun" \
        "$HERE/package/payload/bin/gotatun"
    chmod 755 "$HERE/package/payload/bin/gotatun"
}

mkdir -p "$OUT" "$HERE/package/payload/bin"
# 先清掉另一个后端的文件：package/ 是所有包共用的，上个包留下的 gotatun /
# wireguard.ko 会被打进下一个包（打包器看到 .ko 还会要求声明 compat）。
rm -f "$HERE/package/payload/bin/wireguard.ko" "$HERE/package/payload/bin/gotatun"

build_wg

if [ "$BACKEND" = kernel ]; then
    KO="$VENDOR/prebuilt/aarch64-unknown-linux-musl/wireguard.ko"
    [ -f "$KO" ] || die "缺少 $KO"
    if [ -f "$VENDOR/prebuilt/aarch64-unknown-linux-musl/SHA256SUMS" ] &&
        command -v sha256sum >/dev/null 2>&1; then
        ( cd "$VENDOR/prebuilt/aarch64-unknown-linux-musl" && sha256sum -c --quiet SHA256SUMS ) ||
            die "wireguard.ko 校验失败（$KO）"
    fi
    cp -f "$KO" "$HERE/package/payload/bin/wireguard.ko"
    chmod 644 "$HERE/package/payload/bin/wireguard.ko"
else
    build_gotatun
fi
