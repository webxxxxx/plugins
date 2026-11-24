#!/bin/sh
# 编译 vendor/meow-rs，产物写到 out/<name>；工具链在主机上，源码在项目内。
#
# 交叉目标要两样东西齐全，缺哪样都在这里先说清楚（否则 cargo 只会甩一句
# "can't find crate for core"，或者链接时找不到 gcc）：
#   1. rustup 为该工具链装了目标 —— vendor/meow-rs 的 rust-toolchain.toml 固定了
#      版本，`rustup target add`（默认工具链）装了不算数；
#   2. 对应目标的 C 交叉工具链 —— Linux 上在 /usr/soft/*-cross，Windows 上可用
#      X86_64_MUSL_CROSS / AARCH64_CROSS / CROSS_PREFIX 指到安装目录。
set -eu
NAME=$1; VERSION=$2; PLATFORM=$3
HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
mkdir -p "$HERE/out"
command -v cargo >/dev/null 2>&1 || PATH="$HOME/.cargo/bin:$PATH"   # WSL 的非登录 shell 常缺这个

die() { echo "meow: $*" >&2; exit 1; }

# 交叉工具链的根目录：$1 是环境变量值（可能为空），$2 是编译器名，其余是候选目录。
# 返回第一个真的装着 $2 的目录；一个都没有就失败。
cross_root() {
    _value=$1
    _compiler=$2
    shift 2
    for _dir in "$_value" "$@"; do
        if [ -n "$_dir" ] && { [ -e "$_dir/bin/$_compiler" ] || [ -e "$_dir/bin/$_compiler.exe" ]; }; then
            printf '%s' "$_dir"
            return 0
        fi
    done
    return 1
}

case "$PLATFORM" in
  armv7-unknown-linux-musleabihf)
    CROSS=$(cross_root "${CROSS_PREFIX:-}" arm-linux-musleabihf-gcc /usr/soft/arm-linux-musleabihf-cross) ||
      die "找不到 armv7 交叉工具链：设 CROSS_PREFIX=<root>（里面有 bin/arm-linux-musleabihf-gcc）"
    export PATH="$CROSS/bin:$PATH"
    export CC_armv7_unknown_linux_musleabihf=arm-linux-musleabihf-gcc
    export CXX_armv7_unknown_linux_musleabihf=arm-linux-musleabihf-g++
    export CARGO_TARGET_ARMV7_UNKNOWN_LINUX_MUSLEABIHF_LINKER=arm-linux-musleabihf-gcc
    BINDGEN_ARGS="--target=armv7-unknown-linux-musleabihf --sysroot=$CROSS/arm-linux-musleabihf"
    export BINDGEN_EXTRA_CLANG_ARGS="$BINDGEN_ARGS"
    export BINDGEN_EXTRA_CLANG_ARGS_armv7_unknown_linux_musleabihf="$BINDGEN_ARGS"
    TC=${TMPDIR:-/tmp}/svcd-armv7.cmake
    {
      echo "set(CMAKE_SYSTEM_NAME Linux)"
      echo "set(CMAKE_SYSTEM_PROCESSOR arm)"
      echo "set(CMAKE_C_COMPILER $CROSS/bin/arm-linux-musleabihf-gcc)"
      echo "set(CMAKE_CXX_COMPILER $CROSS/bin/arm-linux-musleabihf-g++)"
      echo "set(CMAKE_AR $CROSS/bin/arm-linux-musleabihf-ar)"
      echo "set(CMAKE_RANLIB $CROSS/bin/arm-linux-musleabihf-ranlib)"
      echo "set(CMAKE_FIND_ROOT_PATH $CROSS)"
      echo "set(CMAKE_FIND_ROOT_PATH_MODE_PROGRAM NEVER)"
      echo "set(CMAKE_FIND_ROOT_PATH_MODE_LIBRARY ONLY)"
      echo "set(CMAKE_FIND_ROOT_PATH_MODE_INCLUDE ONLY)"
    } > "$TC"
    export CMAKE_TOOLCHAIN_FILE=$TC ;;
  aarch64-unknown-linux-musl)
    A=$(cross_root "${AARCH64_CROSS:-}" aarch64-linux-musl-gcc /usr/soft/aarch64-linux-musl-cross) ||
      die "找不到 aarch64 交叉工具链：设 AARCH64_CROSS=<root>（里面有 bin/aarch64-linux-musl-gcc）"
    export PATH="$A/bin:$PATH"
    export CC_aarch64_unknown_linux_musl=aarch64-linux-musl-gcc
    export CXX_aarch64_unknown_linux_musl=aarch64-linux-musl-g++
    export AR_aarch64_unknown_linux_musl=aarch64-linux-musl-ar
    export RANLIB_aarch64_unknown_linux_musl=aarch64-linux-musl-ranlib
    export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=aarch64-linux-musl-gcc
    BINDGEN_ARGS="--target=aarch64-unknown-linux-musl --sysroot=$A/aarch64-linux-musl"
    export BINDGEN_EXTRA_CLANG_ARGS="$BINDGEN_ARGS"
    export BINDGEN_EXTRA_CLANG_ARGS_aarch64_unknown_linux_musl="$BINDGEN_ARGS" ;;
  x86_64-unknown-linux-musl)
    X=$(cross_root "${X86_64_MUSL_CROSS:-${CROSS_PREFIX:-}}" x86_64-linux-musl-gcc \
        /usr/soft/x86_64-linux-musl-cross /d/soft/x86_64-linux-musl) ||
      die "找不到 x86_64 musl 交叉工具链：设 X86_64_MUSL_CROSS=<root>（里面有 bin/x86_64-linux-musl-gcc）"
    export PATH="$X/bin:$PATH"
    export CC_x86_64_unknown_linux_musl=x86_64-linux-musl-gcc
    export CXX_x86_64_unknown_linux_musl=x86_64-linux-musl-g++
    export AR_x86_64_unknown_linux_musl=x86_64-linux-musl-ar
    export RANLIB_x86_64_unknown_linux_musl=x86_64-linux-musl-ranlib
    export CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=x86_64-linux-musl-gcc
    BINDGEN_ARGS="--target=x86_64-unknown-linux-musl --sysroot=$X/x86_64-linux-musl"
    export BINDGEN_EXTRA_CLANG_ARGS="$BINDGEN_ARGS"
    export BINDGEN_EXTRA_CLANG_ARGS_x86_64_unknown_linux_musl="$BINDGEN_ARGS" ;;
  x86_64-unknown-linux-gnu|x86_64-pc-windows-msvc|aarch64-apple-darwin|x86_64-apple-darwin) ;;
  *) echo "unsupported platform: $PLATFORM" >&2; exit 1 ;;
esac
cd "$HERE/vendor/meow-rs"

# 目标没装时 cargo 只会说 "can't find crate for core"，而且 `rustup target add`
# 装到默认工具链上也不算数：rust-toolchain.toml 把版本固定在 pin 上。
require_target() {
    command -v rustup >/dev/null 2>&1 || return 0
    rustup target list --installed 2>/dev/null | grep -qx "$1" && return 0
    _tc=$(rustup show active-toolchain 2>/dev/null | cut -d' ' -f1)
    die "当前工具链（${_tc:-?}）没有 $1 目标；版本由 rust-toolchain.toml 固定，装到默认工具链不算：
  rustup target add --toolchain ${_tc:-stable} $1"
}
require_target "$PLATFORM"
# 在 /mnt/d 上直接编译会因 drvfs 压力 OOM，target 放到 WSL 本地盘。
# 不要移动 rust-toolchain.toml：它决定工具链，移走会让构建版本漂移。
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-${TMPDIR:-/tmp}/svcd-meow-target}
JOBS=${JOBS:-4}
cargo build --release --locked -j "$JOBS" --target "$PLATFORM" --bin meow
case "$PLATFORM" in
  *windows*) cp "$CARGO_TARGET_DIR/$PLATFORM/release/meow.exe" "$HERE/out/$NAME.exe" ;;
  *)         cp "$CARGO_TARGET_DIR/$PLATFORM/release/meow" "$HERE/out/$NAME" ;;
esac

# 入口的平台差异不归配方管：Linux 的 exec/lifecycle 是 meow.sh，Windows 的是
# meow.ps1，那份清单差异写在 platforms/unix.json 与 platforms/windows.json，
# 由 plugin-tools 打包时合并。这里只出一个内核二进制。
