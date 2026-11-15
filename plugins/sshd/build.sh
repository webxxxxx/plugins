#!/bin/sh
# 静态非 PIE：dropbear（vendor/dropbear-2022.83）+ sftp-server（vendor/openssh-9.9p2）。
# 平台到工具链的映射是表驱动的，不写死；CI 用 --platform 逐个传入。
set -eu
NAME=$1; PLATFORM=$3
HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
mkdir -p "$HERE/out"

# 平台 → (HOST 前缀, 交叉工具链目录, 额外 CFLAGS)
case "$PLATFORM" in
  armv7-unknown-linux-musleabihf)
    HOST=arm-linux-musleabihf
    CROSS=${CROSS_PREFIX:-/usr/soft/arm-linux-musleabihf-cross}
    EXTRA="-march=armv7-a -mfpu=vfpv3-d16 -mfloat-abi=hard" ;;
  aarch64-unknown-linux-musl)
    HOST=aarch64-linux-musl
    CROSS=${CROSS_PREFIX:-/usr/soft/aarch64-linux-musl-cross}
    EXTRA="" ;;
  x86_64-unknown-linux-musl)
    HOST=x86_64-linux-musl
    CROSS=${X86_64_MUSL_CROSS:-/usr/soft/x86_64-linux-musl-cross}
    EXTRA="" ;;
  x86_64-unknown-linux-gnu)
    HOST=x86_64-linux-gnu
    CROSS=""            # 本机 gcc 即目标编译器
    EXTRA="" ;;
  aarch64-apple-darwin)
    HOST=aarch64-apple-darwin
    CROSS=""
    EXTRA="-arch arm64" ;;
  x86_64-apple-darwin)
    HOST=x86_64-apple-darwin
    CROSS=""
    EXTRA="-arch x86_64" ;;
  *)
    echo "sshd: $PLATFORM 没有 dropbear 移植（上游无 Windows 版本）" >&2
    exit 1 ;;
esac
[ -n "$CROSS" ] && export PATH="$CROSS/bin:$PATH"
if [ "$HOST" = "x86_64-linux-gnu" ]; then
  CC_CMD=gcc
elif [ "$PLATFORM" = "aarch64-apple-darwin" ] || [ "$PLATFORM" = "x86_64-apple-darwin" ]; then
  CC_CMD=clang
else
  CC_CMD=$HOST-gcc
fi

cd "$HERE/vendor/dropbear-2022.83"
make distclean >/dev/null 2>&1 || true
CC=$CC_CMD CFLAGS="-Os -ffunction-sections -fdata-sections -fno-pie -fno-PIE $EXTRA" \
LDFLAGS="-static -no-pie -Wl,-no-pie -Wl,--gc-sections -Wl,--strip-all" \
./configure --host=$HOST --enable-static --disable-harden --disable-zlib --disable-pam \
  --disable-syslog --disable-lastlog --disable-utmpx --disable-utmp --disable-wtmp \
  --disable-wtmpx --disable-loginfunc --disable-pututline --disable-pututxline
make PROGRAMS="dropbear dropbearkey" MULTI=1 STATIC=1
cp dropbearmulti "$HERE/out/$NAME"

cd "$HERE/vendor/openssh-9.9p2"
make distclean >/dev/null 2>&1 || true
./configure --host=$HOST --disable-shared --enable-static --without-openssl --without-zlib \
  CFLAGS="-O2 -fno-pie $EXTRA" LDFLAGS="-static -no-pie"
make -j"$(nproc)" sftp-server || true
$CC_CMD -o "$HERE/out/sftp-server" sftp-common.o sftp-server.o sftp-server-main.o \
  -static -no-pie -L. -Lopenbsd-compat -lssh -lopenbsd-compat -lssh
${HOST}-strip "$HERE/out/sftp-server" 2>/dev/null || strip "$HERE/out/sftp-server"
