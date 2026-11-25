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
REPO=$(CDPATH= cd -- "$HERE/../.." && pwd)
mkdir -p "$HERE/out"
command -v cargo >/dev/null 2>&1 || PATH="$HOME/.cargo/bin:$PATH"   # WSL 的非登录 shell 常缺这个

die() { echo "meow: $*" >&2; exit 1; }

# ---- 在容器里编：宿主机只要 docker ----
# SVCD_DOCKER=1 时整段编译丢给 tools/docker 的 rust-cross 镜像（clang、libclang、MSVC SDK、
# 交叉工具链都在里面），宿主机不装任何东西。容器里带着 SVCD_IN_DOCKER=1，不会再递归。
if [ "${SVCD_DOCKER:-0}" = 1 ] && [ "${SVCD_IN_DOCKER:-0}" != 1 ]; then
    [ -f "$REPO/tools/docker/docker-build.sh" ] || die "SVCD_DOCKER=1 但找不到 tools/docker/docker-build.sh"
    echo "meow: docker 里编 $PLATFORM" >&2
    # 项目根钉成仓库根（-w 是「根下哪个子目录」，不是「当前目录」）；
    # 容器里的 cwd 就是 /work，所以下面按仓库相对路径找自己
    exec sh "$REPO/tools/docker/docker-build.sh" -r "$REPO" -w . -n meow -- \
        sh plugins/meow/build.sh "$NAME" "$VERSION" "$PLATFORM"
fi

# BoringSSL 会把 google benchmark 一起配进来（BUILD_TESTING 缺省 ON），而 benchmark 的
# 特性探测在 Windows 上很容易失败："Failed to determine the source files for the regular
# expression backend"（本机 CMake 4 + 长路径），交叉编译时也会卡在它的 find_package(Threads)。
# 我们只要 libcrypto/libssl，所以在项目 CMakeLists 之前就用工具链文件把 BUILD_TESTING
# 关掉；include(CTest) 不会覆盖已有的缓存值。
TC=${TMPDIR:-/tmp}/svcd-meow.cmake
cat > "$TC" <<'EOF'
set(BUILD_TESTING OFF CACHE BOOL "meow: no boringssl tests or benchmarks" FORCE)
EOF
export CMAKE_TOOLCHAIN_FILE=$TC

# Windows 上 boring-sys 用 bindgen 生成绑定，bindgen 要 libclang.dll。装了 LLVM
# （scoop 或官方安装包）就把它找出来，省得每个 shell 自己设 LIBCLANG_PATH。
if [ -z "${LIBCLANG_PATH:-}" ]; then
    for _dir in "$HOME/scoop/apps/llvm/current/bin" "/c/Program Files/LLVM/bin" \
        "/c/Program Files (x86)/LLVM/bin"; do
        if [ -e "$_dir/libclang.dll" ]; then
            export LIBCLANG_PATH="$_dir"
            export PATH="$_dir:$PATH"
            break
        fi
    done
fi

# 交叉工具链的映射只有一份：tools/cross-env.sh。PATH、CC_/CXX_/AR_/RANLIB_、
# CARGO_TARGET_*_LINKER 都由它导出；meow 这边只补 BoringSSL 要的两样：
# bindgen 的 sysroot，和 cmake 认的编译器全路径。
. "$REPO/tools/cross-env.sh"
svcd_cross_apply "$PLATFORM" || die "$SVCD_CROSS_ERROR"

case "$PLATFORM" in
  armv7-unknown-linux-musleabihf|aarch64-unknown-linux-musl|x86_64-unknown-linux-musl)
    BINDGEN_ARGS="--target=$PLATFORM"
    # musl-gcc 那条没有 sysroot，硬塞一个反而是错路径
    if [ -n "$SVCD_CROSS_SYSROOT" ]; then
      BINDGEN_ARGS="$BINDGEN_ARGS --sysroot=$SVCD_CROSS_SYSROOT"
    fi
    export BINDGEN_EXTRA_CLANG_ARGS="$BINDGEN_ARGS"
    export "BINDGEN_EXTRA_CLANG_ARGS_$(printf '%s' "$PLATFORM" | tr '-' '_')=$BINDGEN_ARGS" ;;
  x86_64-unknown-linux-gnu|x86_64-pc-windows-msvc|aarch64-apple-darwin|x86_64-apple-darwin) ;;
  *) echo "unsupported platform: $PLATFORM" >&2; exit 1 ;;
esac

# BoringSSL 的 cmake 只认绝对路径，armv7 这一支要显式给。追加到上面那份工具链文件里：
# BUILD_TESTING 那行要留着。
case "$PLATFORM" in
  armv7-unknown-linux-musleabihf)
    {
      echo "set(CMAKE_SYSTEM_NAME Linux)"
      echo "set(CMAKE_SYSTEM_PROCESSOR arm)"
      echo "set(CMAKE_C_COMPILER $SVCD_CROSS_ROOT/bin/$SVCD_CROSS_PREFIX-gcc)"
      echo "set(CMAKE_CXX_COMPILER $SVCD_CROSS_ROOT/bin/$SVCD_CROSS_PREFIX-g++)"
      echo "set(CMAKE_AR $SVCD_CROSS_ROOT/bin/$SVCD_CROSS_PREFIX-ar)"
      echo "set(CMAKE_RANLIB $SVCD_CROSS_ROOT/bin/$SVCD_CROSS_PREFIX-ranlib)"
      echo "set(CMAKE_FIND_ROOT_PATH $SVCD_CROSS_ROOT)"
      echo "set(CMAKE_FIND_ROOT_PATH_MODE_PROGRAM NEVER)"
      echo "set(CMAKE_FIND_ROOT_PATH_MODE_LIBRARY ONLY)"
      echo "set(CMAKE_FIND_ROOT_PATH_MODE_INCLUDE ONLY)"
    } >> "$TC" ;;
  x86_64-pc-windows-msvc)
    # Windows 上要一个自足的 exe：默认是动态 CRT，产物会 import MSVCP140.dll /
    # VCRUNTIME140.dll，机器上没装 VC++ 运行库就起不来，而装插件不该要求用户先装
    # 运行库。Rust 侧在下面用 target-feature=+crt-static，BoringSSL 侧在这里改成
    # /MT；两边混着用会在链接期报 CRT 符号重复。
    echo 'set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreaded" CACHE STRING "" FORCE)' >> "$TC" ;;
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
# 宿主是不是 Windows：plugin-tools 用 `sh` 跑本脚本，Git for Windows 的 sh 报的是
# MSYS_NT-…（只有 bash 才报 MINGW64_NT-…），所以 *_NT-* 这几种都算。这个判断很重要：
# Windows 上就该用本机 MSVC 编，一旦误判成"非 Windows"就会去装 cargo-xwin，用
# clang-cl + CMake/Ninja 交叉编 BoringSSL——CI 上 CMake 读不到 Ninja 版本，整条构建
# 在 configure 阶段就挂。
host_is_windows() {
    case "$(uname -s 2>/dev/null || echo unknown)" in
        *_NT-*|MINGW*|MSYS*|CYGWIN*|Windows*) return 0 ;;
        *) return 1 ;;
    esac
}

case "$PLATFORM" in
  x86_64-pc-windows-msvc)
    if ! host_is_windows && [ -z "${MEOW_MSVC_LINKER:-}" ]; then
      # 非 Windows 宿主：msvc 目标要靠 cargo-xwin。第一次会现装它 + 下 MSVC CRT/SDK
      # （XWIN_CACHE_DIR 指在 tools/docker/run.sh 的工作目录里，之后复用）。
      if ! cargo xwin --version >/dev/null 2>&1; then
        echo "meow: 第一次编 $PLATFORM，先装 cargo-xwin（几分钟）…" >&2
        cargo install cargo-xwin --locked || die "cargo-xwin 装不上（网络？）"
      fi
      # cargo-xwin 自己会给 cmake 传工具链，把上面的 CMAKE_TOOLCHAIN_FILE 顶掉，于是
      # BUILD_TESTING 又回到 ON、google benchmark 在 configure 阶段就失败（交叉时它探测
      # 不了平台）。所以这一支直接把开关钉进 BoringSSL 的 CMakeLists（幂等）。
      _patched=0
      for _f in "${CARGO_HOME:-$HOME/.cargo}"/registry/src/*/boring-sys-*/deps/boringssl/CMakeLists.txt; do
        [ -f "$_f" ] || continue
        grep -q 'meow: BUILD_TESTING OFF' "$_f" && continue
        sed -i '1i set(BUILD_TESTING OFF CACHE BOOL "meow: BUILD_TESTING OFF" FORCE)' "$_f" &&
          _patched=1 && echo "meow: 关掉 BoringSSL 的测试/benchmark（$_f）" >&2
      done
      # 已经解出来的那份也要重新解，否则补丁到不了 cmake 看到的源码树
      [ "$_patched" = 1 ] && rm -rf "${CARGO_TARGET_DIR:?}/$PLATFORM/release/build/boring-sys-"*
      cargo xwin build --release --locked -j "$JOBS" --target "$PLATFORM" --bin meow
    else
      # 本机 MSVC。静态 CRT：exe 不依赖 VCRUNTIME140.dll / MSVCP140.dll，
      # 和上面工具链文件里的 /MT 是一对。
      case "${RUSTFLAGS:-}" in
        *+crt-static*) ;;
        *) export RUSTFLAGS="${RUSTFLAGS:-} -C target-feature=+crt-static" ;;
      esac
      cargo build --release --locked -j "$JOBS" --target "$PLATFORM" --bin meow
    fi ;;
  *) cargo build --release --locked -j "$JOBS" --target "$PLATFORM" --bin meow ;;
esac
case "$PLATFORM" in
  *windows*) cp "$CARGO_TARGET_DIR/$PLATFORM/release/meow.exe" "$HERE/out/$NAME.exe" ;;
  *)         cp "$CARGO_TARGET_DIR/$PLATFORM/release/meow" "$HERE/out/$NAME" ;;
esac

# 入口的平台差异不归配方管：Linux 的 exec/lifecycle 是 meow.sh，Windows 的是
# meow.ps1，那份清单差异写在 platforms/unix.json 与 platforms/windows.json，
# 由 plugin-tools 打包时合并。这里只出一个内核二进制。
