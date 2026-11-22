#!/bin/sh
# 编译 vendor/meow-rs，产物写到 out/<name>；工具链在主机上，源码在项目内。
set -eu
NAME=$1; VERSION=$2; PLATFORM=$3
HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
CROSS=${CROSS_PREFIX:-/usr/soft/arm-linux-musleabihf-cross}
mkdir -p "$HERE/out"
command -v cargo >/dev/null 2>&1 || PATH="$HOME/.cargo/bin:$PATH"   # WSL 的非登录 shell 常缺这个
case "$PLATFORM" in
  armv7-unknown-linux-musleabihf)
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
    A=${AARCH64_CROSS:-/usr/soft/aarch64-linux-musl-cross}
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
    X=${X86_64_MUSL_CROSS:-${CROSS_PREFIX:-/usr/soft/x86_64-linux-musl-cross}}
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
# 在 /mnt/d 上直接编译会因 drvfs 压力 OOM，target 放到 WSL 本地盘。
# 不要移动 rust-toolchain.toml：它决定工具链，移走会让构建版本漂移。
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-${TMPDIR:-/tmp}/svcd-meow-target}
JOBS=${JOBS:-4}
cargo build --release --locked -j "$JOBS" --target "$PLATFORM" --bin meow
cp "$CARGO_TARGET_DIR/$PLATFORM/release/meow" "$HERE/out/$NAME"
