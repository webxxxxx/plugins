#!/bin/sh
# 编译 vendor/meow-rs，产物写到 out/<name>；工具链在主机上，源码在项目内。
set -eu
NAME=$1; VERSION=$2; PLATFORM=$3
HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
CROSS=${CROSS_PREFIX:-/usr/soft/arm-linux-musleabihf-cross}
mkdir -p "$HERE/out"
case "$PLATFORM" in
  armv7-unknown-linux-musleabihf)
    export PATH="$CROSS/bin:$PATH"
    export CC_armv7_unknown_linux_musleabihf=arm-linux-musleabihf-gcc
    export CXX_armv7_unknown_linux_musleabihf=arm-linux-musleabihf-g++
    export CARGO_TARGET_ARMV7_UNKNOWN_LINUX_MUSLEABIHF_LINKER=arm-linux-musleabihf-gcc
    TC=/tmp/svcd-armv7.cmake
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
    export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=aarch64-linux-musl-gcc ;;
  x86_64-unknown-linux-musl|x86_64-unknown-linux-gnu|x86_64-pc-windows-msvc|aarch64-apple-darwin|x86_64-apple-darwin) ;;
  *) echo "unsupported platform: $PLATFORM" >&2; exit 1 ;;
esac
cd "$HERE/vendor/meow-rs"
[ -f rust-toolchain.toml ] && mv rust-toolchain.toml rust-toolchain.toml.bak
cargo build --release --target "$PLATFORM"
cp "target/$PLATFORM/release/meow" "$HERE/out/$NAME"
