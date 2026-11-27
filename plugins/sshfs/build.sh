#!/bin/sh
# 打包前从源码编译 sshfs 与它的一整条静态依赖链（pcre2 / libffi / zlib / glib / libfuse3），
# 再加 dropbear 的 dbclient + dropbearkey。
#
# 配方搬自 sr6110_rebuild（thirdparty/sshfs/build.sh + thirdparty/dropbear/build.sh），
# 只改了三处：
#   * 工具链走仓库唯一的表 tools/cross-env.sh，不再写死 ~/sr6110_fresh；
#   * 依赖前缀落在 out/prefix（上游用 ~/sshfs-static），构建产物不散到宿主；
#   * 每次都从干净的 out/build 开始，和上游一致，避免陈旧中间产物。
#
# 平台：Linux 四个三元组。musl 的三个做全静态；x86_64-gnu 动态链接 glibc
# （静态 glibc 的 getaddrinfo 要走 libnss_dns.so，解析域名会失败）。
# 上游 sshfs 也支持 macOS（macFUSE），但那是另一套 mount helper / 内核扩展，
# 不在这个包的自包含范围里；Windows 没有 FUSE，插件本体也跑不了 sh。
set -eu
NAME=${1:-sshfs}
VERSION=${2:-0.0.0}
PLATFORM=${3:?platform is required}
HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPO=$(CDPATH= cd -- "$HERE/../.." && pwd)
SRC="$HERE/vendor"
OUT="$HERE/out"
BUILD="$OUT/build"
PREFIX="$OUT/prefix"

die() { echo "sshfs: $*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "缺少 $1（$2）"; }

check_lf() {
    for f in "$HERE/package/payload/bin"/*.sh "$HERE/package/config/config"; do
        [ -f "$f" ] || continue
        if grep -q "$(printf '\r')" "$f" 2>/dev/null; then
            die "$f 里有 CRLF；payload 脚本/配置必须用 LF"
        fi
    done
}
check_lf

# 三元组 -> autoconf --host / meson 的 host_machine / 是否全静态。
case "$PLATFORM" in
x86_64-unknown-linux-musl)
    AC_HOST=x86_64-linux
    CPU_FAMILY=x86_64
    CPU=x86_64
    STATIC=1
    ;;
aarch64-unknown-linux-musl)
    AC_HOST=aarch64-linux
    CPU_FAMILY=aarch64
    CPU=aarch64
    STATIC=1
    ;;
armv7-unknown-linux-musleabihf)
    AC_HOST=arm-linux
    CPU_FAMILY=arm
    CPU=armv7
    STATIC=1
    ;;
x86_64-unknown-linux-gnu)
    AC_HOST=x86_64-linux
    CPU_FAMILY=x86_64
    CPU=x86_64
    # 动态链接 glibc：静态 glibc 的 NSS 解析在运行时还要 dlopen libnss_dns.so，
    # 自包含的包里拿不到；gnu 包本来就要求宿主的 glibc 版本（CI 在 22.04 上编）。
    STATIC=0
    ;;
*)
    die "不支持的平台 $PLATFORM（Linux: x86_64/aarch64/armv7 musl + x86_64 gnu）"
    ;;
esac

for tool in meson ninja pkg-config make tar gzip bzip2 xz; do
    need "$tool" "sshfs 的依赖链要用它（apt install meson ninja-build pkg-config）"
done
# 源码包：七个 tarball 都在 vendor/，缺一个都不行
for tarball in pcre2-10.42.tar.bz2 libffi-3.4.4.tar.gz zlib-1.3.1.tar.gz \
    glib-2.78.4.tar.xz fuse-3.16.2.tar.gz sshfs-3.7.3.tar.gz dropbear-2022.83.tar.bz2; do
    [ -f "$SRC/$tarball" ] || die "缺少 $SRC/$tarball"
done

. "$REPO/tools/cross-env.sh"
svcd_cross_apply "$PLATFORM" || die "cross-env: $SVCD_CROSS_ERROR"
CC=${SVCD_CROSS_CC:-cc}
AR=${SVCD_CROSS_PREFIX:-}-ar
RANLIB=${SVCD_CROSS_PREFIX:-}-ranlib
STRIP=${SVCD_CROSS_PREFIX:-}-strip
[ "$AR" = "-ar" ] && AR=ar
[ "$RANLIB" = "-ranlib" ] && RANLIB=ranlib
[ "$STRIP" = "-strip" ] && STRIP=strip
# 解析成绝对路径再写进 meson cross file：ninja 的 PATH 不一定还挂着工具链，
# 只写名字会在编译期报 "aarch64-linux-musl-gcc: not found"。
for tool in CC AR RANLIB STRIP; do
    eval "resolved=\$(command -v \"\$$tool\" 2>/dev/null || true)"
    [ -n "$resolved" ] || die "找不到 $tool（tools/cross/install.sh $PLATFORM）"
    eval "$tool=\$resolved"
done

# 上游用 ~/.sshfs-static 当 prefix 是为了和设备上已部署的二进制字节级一致；
# 我们的二进制只从 payload/bin 走，prefix 只影响 fusermount3 的默认查找路径
# （libfuse 在 exec 失败后会退回 PATH），所以放 out/ 下即可。
rm -rf "$BUILD" "$PREFIX"
mkdir -p "$BUILD" "$PREFIX" "$OUT"
# 上一次构建留在 payload/bin 和 out/ 的四个二进制先清掉，避免新旧混着进包
rm -f "$OUT/sshfs" "$OUT/fusermount3" "$OUT/dbclient" "$OUT/dropbearkey"
rm -f "$HERE/package/payload/bin/sshfs" "$HERE/package/payload/bin/fusermount3" \
    "$HERE/package/payload/bin/dbclient" "$HERE/package/payload/bin/dropbearkey"

if [ "$STATIC" = 1 ]; then
    LINK_ARGS='-Dc_link_args=-static'
    DROPBEAR_LDFLAGS='-static'
else
    LINK_ARGS=''
    DROPBEAR_LDFLAGS=''
fi

cat > "$BUILD/cross.txt" <<EOF
[binaries]
c = '$CC'
ar = '$AR'
strip = '$STRIP'
pkgconfig = 'pkg-config'

[host_machine]
system = 'linux'
cpu_family = '$CPU_FAMILY'
cpu = '$CPU'
endian = 'little'
EOF

# 全静态目标把“编译器自检”也静态链接：x86_64 musl 的测试程序会在构建机上真的
# 被执行（build/host 同架构），动态链接的 musl 测试程序找不到 loader，glib 的
# gnulib 检查会把 frexpl 误判成 broken。aarch64/armv7 不会被执行，写上也无害。
if [ "$STATIC" = 1 ]; then
    cat >> "$BUILD/cross.txt" <<EOF

[built-in options]
c_link_args = ['-static']
EOF
fi

export PKG_CONFIG_PATH="$PREFIX/lib/pkgconfig"
export PKG_CONFIG_LIBDIR="$PREFIX/lib/pkgconfig"
export PKG_CONFIG_SYSROOT_DIR=

cd "$BUILD"

echo "sshfs: 1/7 pcre2 ($PLATFORM)" >&2
tar xf "$SRC/pcre2-10.42.tar.bz2"
(
    cd pcre2-10.42
    ./configure --host="$AC_HOST" --prefix="$PREFIX" --disable-shared --enable-static \
        CC="$CC" AR="$AR" RANLIB="$RANLIB" >/dev/null
    make -j"$(nproc)" >/dev/null
    make install >/dev/null
)

echo "sshfs: 2/7 libffi ($PLATFORM)" >&2
tar xf "$SRC/libffi-3.4.4.tar.gz"
(
    cd libffi-3.4.4
    # --disable-exec-static-tramp：musl 上必须；--disable-builddir：避免 configure
    # 在子目录里 re-exec（drvfs 上会报 config.log: No such file or directory）。
    ./configure --host="$AC_HOST" --prefix="$PREFIX" --disable-shared --enable-static \
        --disable-exec-static-tramp --disable-builddir \
        CC="$CC" AR="$AR" RANLIB="$RANLIB" >/dev/null
    make -j"$(nproc)" >/dev/null
    make install >/dev/null
)

echo "sshfs: 3/7 zlib ($PLATFORM)" >&2
tar xf "$SRC/zlib-1.3.1.tar.gz"
(
    cd zlib-1.3.1
    # glib 的 gio 硬依赖 zlib；不要让它去 wrapdb 下载（--wrap-mode=nodownload 会挡住）。
    CC="$CC" AR="$AR" RANLIB="$RANLIB" ./configure --prefix="$PREFIX" --static >/dev/null
    make -j"$(nproc)" >/dev/null
    make install >/dev/null
)

echo "sshfs: 4/7 glib ($PLATFORM)" >&2
tar xf "$SRC/glib-2.78.4.tar.xz"
(
    cd glib-2.78.4
    meson setup _b --cross-file "$BUILD/cross.txt" --prefix="$PREFIX" \
        --default-library=static --wrap-mode=nodownload \
        -Dnls=disabled -Dtests=false -Dlibmount=disabled -Dselinux=disabled -Dman=false >/dev/null
    ninja -C _b >/dev/null
    ninja -C _b install >/dev/null
)

echo "sshfs: 5/7 libfuse3 ($PLATFORM)" >&2
tar xf "$SRC/fuse-3.16.2.tar.gz"
(
    cd fuse-3.16.2
    # -Duseroot=false：安装脚本默认会 chown root:root + chmod u+s fusermount3，
    # 非 root 构建机直接失败；-Dinitscriptdir=：否则会往宿主 /etc/init.d 写文件。
    meson setup _b --cross-file "$BUILD/cross.txt" --prefix="$PREFIX" \
        --default-library=static --wrap-mode=nodownload $LINK_ARGS \
        -Dutils=true -Dexamples=false -Dtests=false \
        -Duseroot=false -Dinitscriptdir= >/dev/null
    ninja -C _b >/dev/null
    ninja -C _b install >/dev/null
)

echo "sshfs: 6/7 sshfs ($PLATFORM)" >&2
tar xf "$SRC/sshfs-3.7.3.tar.gz"
(
    cd sshfs-sshfs-3.7.3
    meson setup _b --cross-file "$BUILD/cross.txt" --prefix="$PREFIX" \
        --default-library=static --wrap-mode=nodownload $LINK_ARGS >/dev/null
    ninja -C _b >/dev/null
)
cp -f sshfs-sshfs-3.7.3/_b/sshfs "$OUT/sshfs"
"$STRIP" "$OUT/sshfs"
cp -f "$PREFIX/bin/fusermount3" "$OUT/fusermount3"
"$STRIP" "$OUT/fusermount3"

echo "sshfs: 7/7 dropbear ($PLATFORM)" >&2
tar xf "$SRC/dropbear-2022.83.tar.bz2"
(
    cd dropbear-2022.83
    ./configure --host="$AC_HOST" \
        --disable-zlib --disable-syslog --disable-lastlog \
        --disable-utmp --disable-utmpx --disable-wtmp --disable-wtmpx \
        --disable-pututline --disable-pututxline CC="$CC" >/dev/null
    # LDFLAGS 必须在 make 命令行给：configure 之后不显式传 -static 会产出动态链接。
    # CFLAGS 绝不能上命令行，否则会盖掉子目录里的 `CFLAGS += -I../`（libtommath 找不到 dbmalloc.h）。
    make PROGRAMS="dbclient dropbearkey" CC="$CC" LDFLAGS="$DROPBEAR_LDFLAGS" >/dev/null
)
cp -f dropbear-2022.83/dbclient dropbear-2022.83/dropbearkey "$OUT/"
"$STRIP" "$OUT/dbclient" "$OUT/dropbearkey"

# sshfs 由 build.json 的 artifact 复制进 payload/bin；另外三个直接放进去。
mkdir -p "$HERE/package/payload/bin"
cp -f "$OUT/fusermount3" "$HERE/package/payload/bin/fusermount3"
cp -f "$OUT/dbclient" "$HERE/package/payload/bin/dbclient"
cp -f "$OUT/dropbearkey" "$HERE/package/payload/bin/dropbearkey"
chmod 755 "$OUT/sshfs" "$HERE/package/payload/bin/fusermount3" \
    "$HERE/package/payload/bin/dbclient" "$HERE/package/payload/bin/dropbearkey"
