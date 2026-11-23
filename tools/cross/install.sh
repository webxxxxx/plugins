#!/bin/sh
# 准备 musl 交叉工具链。
#
#   tools/cross/install.sh <triple> [缓存目录]
#
# 三个 musl 目标各有一个 musl.cc 预编译包；这个脚本负责下载、解包、校验，然后
# 把构建需要的变量交代出去：
#
#   * 在 CI 里（$GITHUB_ENV / $GITHUB_PATH 存在）写进环境文件，后续步骤直接可用；
#   * 本地跑就把 `export` 行打到 stdout，用 `eval "$(tools/cross/install.sh ...)"`。
#
# 变量名跟 release 流水线里 meow / sshd / plugin-store 的构建脚本对齐：
# `CARGO_TARGET_<TRIPLE>_LINKER`、`CC_<triple>`、`CXX_`、`AR_`、`RANLIB_`，
# 外加 `CROSS_PREFIX` / `AARCH64_CROSS` / `X86_64_MUSL_CROSS` 三个历史变量，
# 以及给 build.json 里 `{cc}` 用的 `CC`。
set -eu

usage() {
    cat >&2 <<'EOF'
usage: tools/cross/install.sh <triple> [cache dir]

triples: x86_64-unknown-linux-musl
         aarch64-unknown-linux-musl
         armv7-unknown-linux-musleabihf

The toolchain lands in <cache dir> (default $SVCD_CROSS_DIR or ~/.svcd-cross)
and is reused on the next run.
EOF
}

triple=${1:-}
cache=${2:-${SVCD_CROSS_DIR:-$HOME/.svcd-cross}}
[ -n "$triple" ] || { usage; exit 2; }

case "$triple" in
    x86_64-unknown-linux-musl)
        dir=x86_64-linux-musl-cross
        cc=x86_64-linux-musl-gcc
        triple_upper=X86_64_UNKNOWN_LINUX_MUSL
        triple_lower=x86_64_unknown_linux_musl
        legacy=X86_64_MUSL_CROSS
        ;;
    aarch64-unknown-linux-musl)
        dir=aarch64-linux-musl-cross
        cc=aarch64-linux-musl-gcc
        triple_upper=AARCH64_UNKNOWN_LINUX_MUSL
        triple_lower=aarch64_unknown_linux_musl
        legacy=AARCH64_CROSS
        ;;
    armv7-unknown-linux-musleabihf)
        dir=arm-linux-musleabihf-cross
        cc=arm-linux-musleabihf-gcc
        triple_upper=ARMV7_UNKNOWN_LINUX_MUSLEABIHF
        triple_lower=armv7_unknown_linux_musleabihf
        legacy=CROSS_PREFIX
        ;;
    *)
        echo "cross: no musl toolchain for $triple" >&2
        usage
        exit 2
        ;;
esac

case "$(uname -s 2>/dev/null || echo unknown)" in
    MINGW* | MSYS* | CYGWIN*)
        echo "cross: musl.cc 的包是 Linux 二进制，Windows 上先换 WSL 或 Linux 主机；" >&2
        echo "       只有现成的交叉 gcc 时，直接把 CC 指向它、跳过这一步。" >&2
        exit 2
        ;;
    *) ;;
esac

bin="$cache/$dir/bin/$cc"
if [ ! -x "$bin" ]; then
    mkdir -p "$cache"
    tgz="$cache/$dir.tgz"
    ok=0
    # musl.cc 在 GitHub runner 上不可达，优先用 GitHub Release 镜像，最后回退主站。
    for base in \
        https://github.com/musl-cc/musl.cc/releases/download/v0.0.1 \
        https://github.com/tsl0922/musl-toolchains/releases/download/2021-11-23 \
        https://musl.cc; do
        if curl -fL --connect-timeout 10 --max-time 600 --retry 2 --retry-delay 3 -o "$tgz" "$base/$dir.tgz" \
            && tar tzf "$tgz" >/dev/null 2>&1; then
            ok=1
            break
        fi
        rm -f "$tgz"
    done
    if [ "$ok" != 1 ]; then
        echo "cross: could not download $dir.tgz (tried musl.cc and its GitHub mirrors)" >&2
        exit 1
    fi
    tar xzf "$tgz" -C "$cache"
    rm -f "$tgz"
fi
[ -x "$bin" ] || { echo "cross: $bin is missing after unpacking" >&2; exit 1; }

prefix="$cache/$dir"
triple_gcc="$prefix/bin/$cc"
triple_cxx="$prefix/bin/${cc%gcc}g++"
triple_ar="$prefix/bin/${cc%gcc}ar"
triple_ranlib="$prefix/bin/${cc%gcc}ranlib"

# In CI the runner wants the file formats; locally the caller wants to eval.
emit() {
    if [ -n "${GITHUB_ENV:-}" ]; then
        printf '%s=%s\n' "$1" "$2" >>"$GITHUB_ENV"
    else
        printf 'export %s=%s\n' "$1" "$2"
    fi
}

emit_path() {
    if [ -n "${GITHUB_PATH:-}" ]; then
        printf '%s\n' "$1" >>"$GITHUB_PATH"
    else
        printf 'export PATH=%s:"$PATH"\n' "$1"
    fi
}

emit "CARGO_TARGET_${triple_upper}_LINKER" "$triple_gcc"
emit "CC_${triple_lower}" "$triple_gcc"
emit "CXX_${triple_lower}" "$triple_cxx"
emit "AR_${triple_lower}" "$triple_ar"
emit "RANLIB_${triple_lower}" "$triple_ranlib"
# 构建脚本写的三个历史变量：CROSS_PREFIX 一直是「当前平台的那份」，meow 与
# sshd 的 build.sh 就近取它；另外两个按平台各一份。
emit "CROSS_PREFIX" "$prefix"
emit "$legacy" "$prefix"
# `{cc}` in build.json: the C recipe names the compiler this way, because the
# command array cannot vary by platform.
emit "CC" "$triple_gcc"
emit_path "$prefix/bin"

if [ -z "${GITHUB_ENV:-}" ]; then
    echo "export SVCD_CROSS_PREFIX=$prefix" >&2
fi
