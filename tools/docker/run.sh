#!/bin/sh
# 在 rust-cross 镜像里跑任意命令（编 meow、编 plugin-tools、编宿主 svcd、编别的 Rust 插件）。
#
#   sh tools/docker/run.sh -- cargo build --release --target x86_64-pc-windows-msvc
#   sh tools/docker/run.sh -w plugins/meow -- sh plugins/meow/build.sh meow 1.4.0 aarch64-unknown-linux-musl
#   sh tools/docker/run.sh -w tools/cli -n plugin-tools -- cargo build --release --locked
#
# Rust 工具链不装在镜像里，而是把宿主机的挂进去（版本由各仓库的 rust-toolchain.toml 决定，
# 目标也要在宿主机上装好：rustup target add <triple>）。
#
# 挂载（路径都在 WSL 里，别用 /mnt/d 当编译目录：drvfs 又慢又容易把内存吃光）：
#   <仓库>                    → /work        源码；产物写回 out/、dist/
#   ~/.rustup                 → /rustup      宿主机的工具链（可写：rustup 解析 pin 时要落临时文件）
#   ~/.cargo/bin              → /rustbin     cargo / rustup / cargo-xwin 的入口
#   /usr/soft                 → /usr/soft    宿主的交叉工具链（aarch64 / armv7 musl），共用
#   /usr/soft/build_workspace → 同一路径     registry 缓存、每个项目的 target、xwin 缓存
#
# 选项：
#   -w DIR    在仓库的哪个子目录干活（默认仓库根）
#   -n NAME   构建工作目录名（默认取 -w 的目录名）：target 落在 <workspace>/<NAME>/target
#   -t IMAGE  镜像（默认 rust-cross:1.98.1）
#   -j N      并行度（默认 4）
set -eu

REPO=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
WORKSPACE=${SVCD_BUILD_WORKSPACE:-/usr/soft/build_workspace}
IMAGE=${SVCD_CROSS_IMAGE:-rust-cross:1.98.1}
JOBS=${JOBS:-4}
DIR=.
NAME=

while [ $# -gt 0 ]; do
    case "$1" in
    -w) DIR=$2; shift 2 ;;
    -n) NAME=$2; shift 2 ;;
    -t) IMAGE=$2; shift 2 ;;
    -j) JOBS=$2; shift 2 ;;
    --) shift; break ;;
    -*) echo "run: 未知选项 $1" >&2; exit 2 ;;
    *) break ;;
    esac
done
[ $# -gt 0 ] || { echo "run: 缺命令（写在 -- 后面）" >&2; exit 2; }
[ -n "$NAME" ] || NAME=$(basename "$(CDPATH= cd -- "$REPO/$DIR" && pwd)")

command -v docker >/dev/null 2>&1 || {
    echo "run: 找不到 docker（WSL：apt install docker.io && sudo usermod -aG docker \$USER，重开会话）" >&2
    exit 1
}
docker image inspect "$IMAGE" >/dev/null 2>&1 || {
    echo "run: 还没有镜像 $IMAGE" >&2
    echo "     先构建：docker build -f tools/docker/rust-cross.Dockerfile -t $IMAGE '$REPO/tools/docker'" >&2
    exit 1
}

# 宿主机工具链：容器里没有 rustc，全部从这里来
RUSTUP_HOME_HOST=${RUSTUP_HOME:-$HOME/.rustup}
CARGO_BIN_HOST=${CARGO_BIN:-$HOME/.cargo/bin}
[ -d "$RUSTUP_HOME_HOST/toolchains" ] || {
    echo "run: 宿主机没有 rustup 工具链（$RUSTUP_HOME_HOST）" >&2
    echo "     装一个：curl https://sh.rustup.rs -sSf | sh -s -- -y --profile minimal" >&2
    exit 1
}
[ -x "$CARGO_BIN_HOST/cargo" ] || {
    echo "run: 找不到 $CARGO_BIN_HOST/cargo" >&2
    exit 1
}

mkdir -p "$WORKSPACE/cargo" "$WORKSPACE/$NAME/target"
# cargo 的 registry 缓存和镜像配置都放工作目录：第二次编译快很多
if [ ! -f "$WORKSPACE/cargo/config.toml" ]; then
    printf '%s\n' \
        '[source.crates-io]' \
        'replace-with = "rsproxy-sparse"' \
        '' \
        '[source.rsproxy-sparse]' \
        'registry = "sparse+https://rsproxy.cn/index/"' \
        '' \
        '[net]' \
        'git-fetch-with-cli = true' \
        > "$WORKSPACE/cargo/config.toml"
fi

# /usr/soft 挂进同名路径：build.sh 里的 /usr/soft/*-cross 探测因此不用改。
SOFT=
if [ -d /usr/soft ]; then SOFT="-v /usr/soft:/usr/soft"; fi

exec docker run --rm \
    --user "$(id -u):$(id -g)" \
    -e HOME=/tmp \
    -w "/work/$DIR" \
    -v "$REPO:/work" \
    -v "$RUSTUP_HOME_HOST:/rustup" \
    -v "$CARGO_BIN_HOST:/rustbin:ro" \
    -v "$WORKSPACE:$WORKSPACE" \
    $SOFT \
    -e PATH="/rustbin:$WORKSPACE/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin" \
    -e RUSTUP_HOME=/rustup \
    -e CARGO_HOME="$WORKSPACE/cargo" \
    -e CARGO_TARGET_DIR="$WORKSPACE/$NAME/target" \
    -e XWIN_CACHE_DIR="$WORKSPACE/xwin" \
    -e XWIN_ACCEPT_LICENSE=1 \
    -e SVCD_IN_DOCKER=1 \
    -e JOBS="$JOBS" \
    "$IMAGE" "$@"
