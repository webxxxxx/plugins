#!/bin/sh
# 目标三元组 → C 交叉工具链，唯一一张表。三处共用，不各抄一份：
#
#   tools/docker/docker-build.sh    容器里：sh -c '. /svcd-tools/cross-env.sh && svcd_cross_exec "$0" "$@"'
#   plugins/meow/build.sh           . "$REPO/tools/cross-env.sh"; svcd_cross_apply "$PLATFORM"
#   plugins/plugin-store/build.sh   同上
#
# 用法：
#   . tools/cross-env.sh
#   svcd_cross_apply armv7-unknown-linux-musleabihf     # 0=配好；1=缺工具链，看 $SVCD_CROSS_ERROR
#   svcd_cross_exec  armv7-unknown-linux-musleabihf cargo build --release
#
# 认的目标（其它目标当原生处理，什么都不做）：
#   armv7-unknown-linux-musleabihf  /usr/soft/arm-linux-musleabihf-cross    CROSS_PREFIX
#   aarch64-unknown-linux-musl      /usr/soft/aarch64-linux-musl-cross      AARCH64_CROSS
#   x86_64-unknown-linux-musl       /usr/soft/x86_64-linux-musl-cross      X86_64_MUSL_CROSS
#                                   没有整份工具链才退到 musl-gcc（musl-tools），只够纯 C
#
# 配好之后：PATH 前面挂上工具链的 bin，导出 CC_/CXX_/AR_/RANLIB_<triple> 和
# CARGO_TARGET_<TRIPLE>_LINKER；另外留三个变量给需要 sysroot 的调用方
# （meow 的 bindgen、cmake）：SVCD_CROSS_ROOT / SVCD_CROSS_PREFIX / SVCD_CROSS_SYSROOT。
# 退到 musl-gcc 的那条没有根目录，SVCD_CROSS_ROOT、SVCD_CROSS_SYSROOT 是空的。
#
# x86_64 musl 为什么要整份工具链而不是 musl-gcc：musl-gcc 只是裹了 -specs 的 C
# 驱动，C++ 得用宿主机 g++，那是 glibc 的头 + glibc 的 libstdc++.a，跟 musl 的
# libc 焊不到一起（undefined __isoc23_strtoull / fopen64 / __libc_single_threaded）。
# musl.cc 那份工具链在 <root>/x86_64-linux-musl/lib 下带着 musl 版 libstdc++.a。
# 装法：sh tools/cross/install.sh x86_64-unknown-linux-musl /usr/soft
#
# 这里不 set -eu：它是被 source 进来的库，别改调用方的 shell 选项。

# $1=编译器名，其余=候选根；打印第一个装着它的根（.exe 也认，MSYS 上的工具链是那样）
svcd_cross_root() {
    svcd_cross_compiler=$1
    shift
    for svcd_cross_dir in "$@"; do
        [ -n "$svcd_cross_dir" ] || continue
        if [ -e "$svcd_cross_dir/bin/$svcd_cross_compiler" ] ||
            [ -e "$svcd_cross_dir/bin/$svcd_cross_compiler.exe" ]; then
            printf '%s' "$svcd_cross_dir"
            return 0
        fi
    done
    return 1
}

# $1=变量前缀（CC/CXX/AR/RANLIB），$2=值；两种大小写各导出一份。
# 规范名是 CC_armv7_unknown_linux_musleabihf，cc-rs 那个位置接受小写；全大写那份
# 是老表里就在用的写法，一起留着，免得哪天某个工具只认其中一种。
svcd_cross_export() {
    export "$1_$svcd_cross_lower=$2"
    export "$1_$svcd_cross_upper=$2"
}

# $1=目标三元组。配环境并返回 0；目标要交叉工具链但本机没有，设 SVCD_CROSS_ERROR 返回 1。
svcd_cross_apply() {
    svcd_cross_t=${1:-}
    svcd_cross_prefix=
    svcd_cross_cc=
    svcd_cross_cxx=
    svcd_cross_ar=
    svcd_cross_ranlib=
    svcd_cross_root=
    svcd_cross_error=
    SVCD_CROSS_ROOT=
    SVCD_CROSS_PREFIX=
    SVCD_CROSS_SYSROOT=
    SVCD_CROSS_CC=
    SVCD_CROSS_ERROR=

    case "$svcd_cross_t" in
    armv7-unknown-linux-musleabihf)
        svcd_cross_prefix=arm-linux-musleabihf
        svcd_cross_root=$(svcd_cross_root "$svcd_cross_prefix-gcc" "${CROSS_PREFIX:-}" /usr/soft/arm-linux-musleabihf-cross) || svcd_cross_root=
        svcd_cross_error="找不到 $svcd_cross_prefix-gcc：装到 /usr/soft/arm-linux-musleabihf-cross，或设 CROSS_PREFIX=<root>（里面有 bin/$svcd_cross_prefix-gcc）"
        ;;
    aarch64-unknown-linux-musl)
        svcd_cross_prefix=aarch64-linux-musl
        svcd_cross_root=$(svcd_cross_root "$svcd_cross_prefix-gcc" "${AARCH64_CROSS:-}" /usr/soft/aarch64-linux-musl-cross) || svcd_cross_root=
        svcd_cross_error="找不到 $svcd_cross_prefix-gcc：装到 /usr/soft/aarch64-linux-musl-cross，或设 AARCH64_CROSS=<root>"
        ;;
    x86_64-unknown-linux-musl)
        # 整份工具链优先：C 和 C++（BoringSSL/quiche）都能编，标准库是 musl 版。
        svcd_cross_prefix=x86_64-linux-musl
        svcd_cross_root=$(svcd_cross_root "$svcd_cross_prefix-gcc" \
            "${X86_64_MUSL_CROSS:-${CROSS_PREFIX:-}}" \
            /usr/soft/x86_64-linux-musl-cross /usr/soft/x86_64-linux-musl \
            /usr/local/x86_64-linux-musl-cross \
            /d/soft/x86_64-linux-musl) || svcd_cross_root=
        if [ -z "$svcd_cross_root" ]; then
            if command -v musl-gcc >/dev/null 2>&1; then
                # musl-tools：C 驱动 + 链接启动文件，纯 C 目标够用；C++ 会退到宿主机
                # g++（那头是 glibc 的标准库），链接期必炸。只当兜底，并且说出来。
                echo "cross-env: 只有 musl-gcc（musl-tools），没有整份 x86_64-linux-musl 工具链；" >&2
                echo "cross-env: 纯 C 目标能编，带 C++ 依赖的（meow）会链不上。装整份：" >&2
                echo "cross-env:   sh tools/cross/install.sh x86_64-unknown-linux-musl /usr/soft" >&2
                svcd_cross_prefix=musl
                svcd_cross_cc=musl-gcc
                svcd_cross_cxx=g++
            else
                svcd_cross_error="找不到 $svcd_cross_prefix-gcc，也没有 musl-gcc：sh tools/cross/install.sh x86_64-unknown-linux-musl /usr/soft 装一份，或放到 /usr/soft/x86_64-linux-musl-cross，或设 X86_64_MUSL_CROSS=<root>"
            fi
        fi
        ;;
    *)
        return 0 # 原生目标；windows-msvc 由 cargo-xwin 自己管
        ;;
    esac

    if [ -z "$svcd_cross_root" ] && [ -z "$svcd_cross_cc" ]; then
        SVCD_CROSS_ERROR=$svcd_cross_error
        return 1
    fi

    if [ -n "$svcd_cross_root" ]; then
        svcd_cross_cc=$svcd_cross_prefix-gcc
        svcd_cross_cxx=$svcd_cross_prefix-g++
        svcd_cross_ar=$svcd_cross_prefix-ar
        svcd_cross_ranlib=$svcd_cross_prefix-ranlib
        PATH="$svcd_cross_root/bin:$PATH"
        export PATH
        SVCD_CROSS_ROOT=$svcd_cross_root
        SVCD_CROSS_SYSROOT="$svcd_cross_root/$svcd_cross_prefix"
    fi
    SVCD_CROSS_PREFIX=$svcd_cross_prefix
    SVCD_CROSS_CC=$svcd_cross_cc
    export SVCD_CROSS_ROOT SVCD_CROSS_PREFIX SVCD_CROSS_SYSROOT SVCD_CROSS_CC

    # 变量名两份：cc-rs 认 CC_<triple>，也认全大写那份，两个都导出省得猜
    svcd_cross_lower=$(printf '%s' "$svcd_cross_t" | sed 's/-/_/g')
    svcd_cross_upper=$(printf '%s' "$svcd_cross_lower" | tr 'a-z' 'A-Z')

    svcd_cross_export CC "$svcd_cross_cc"
    export "CARGO_TARGET_${svcd_cross_upper}_LINKER=$svcd_cross_cc"
    if [ -n "$svcd_cross_ar" ]; then svcd_cross_export AR "$svcd_cross_ar"; fi
    if [ -n "$svcd_cross_cxx" ]; then svcd_cross_export CXX "$svcd_cross_cxx"; fi
    if [ -n "$svcd_cross_ranlib" ]; then svcd_cross_export RANLIB "$svcd_cross_ranlib"; fi
    return 0
}

# $1=目标三元组（空就跳过），其余=要跑的命令；配好环境后 exec 它。
svcd_cross_exec() {
    svcd_cross_t=${1:-}
    if [ $# -gt 0 ]; then shift; fi
    if [ -n "$svcd_cross_t" ]; then
        svcd_cross_apply "$svcd_cross_t" || {
            echo "cross-env: $SVCD_CROSS_ERROR" >&2
            exit 1
        }
    fi
    exec "$@"
}
