#!/bin/sh
# sshfs: 通过 sshfs 挂载 NAS。
#
# 宿主监督的进程就是 sshfs 本身（exec.args = start）：sshfs 前台运行，
# 连接断了/进程被终止时挂载随之消失或变成失效挂载，宿主看到的始终是它。
#   exec.ready=exec：没有协议通道，spawn 成功即 ready
#   lifecycle.stop ：进程消失之后收尾，用包里的 fusermount3 卸载（SIGKILL
#                    之后挂载点也会留下，必须再卸一次）
#   lifecycle.status：一行 JSON，供 svc status 的 probe 显示
#
# 旧版 start() 还负责"把装在 NAS 上的插件拉起来"。标准插件里没有这条隐藏
# 依赖，改用清单的 dependencies（例如需要 NAS 的插件写 dependencies: ["sshfs"]），
# 由宿主的依赖图保证先起 nas、先停依赖它的插件。
set -u

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
# 宿主给插件的安装目录；手跑时按脚本位置往上三层推（<插件>/<版本>/payload/bin/…）
PLUGIN_ROOT=$(CDPATH= cd -- "${SVCD_PLUGIN_DIR:-$HERE/../../..}" && pwd)
# 插件自己的东西只放这里：宿主升级、卸载（不带 --purge）都保留它。
DATA="$PLUGIN_ROOT/data"

# 系统命令走 PATH；插件二进制只从包内 payload/bin 取。payload/bin 也放进 PATH：
# libfuse 在直接 mount 失败时会按名字 exec fusermount3，非 root 运行时就靠这一条。
PATH=$HERE:/etc/sr/bin:/sbin:/usr/sbin:/bin:/usr/bin
export PATH

# 只读宿主复制过来的插件配置（<插件目录>/config）。
CONF="$PLUGIN_ROOT/config"
[ -f "$CONF" ] && . "$CONF"

log() { echo "sshfs: $*" >&2; }
fail() { log "$*"; return 1; }

# 连接参数没有默认值：必须由插件配置给出（这些值跟现场环境走）。
# 挂载点（SSHFS_MNT）和密钥路径（SSHFS_KEY）不强制，未填时用插件目录内的默认值。
missing=""
for name in SSHFS_HOST SSHFS_USER SSHFS_PATH SSHFS_PORT; do
    eval "value=\${$name:-}"
    [ -n "$value" ] || missing="$missing $name"
done
if [ -n "$missing" ]; then
    log "配置缺少:$missing（见 $CONF；这几项没有默认值）"
    exit 1
fi

HOST=$SSHFS_HOST
NUSER=$SSHFS_USER
RPATH=$SSHFS_PATH
PORT=$SSHFS_PORT
ALIVE=${SSHFS_ALIVE:-15}

# 相对路径按插件目录解释，绝对路径原样用：挂载点和密钥都可以由用户指到别处。
# 没填时落在插件目录内（挂载点 data/mnt，密钥 data/ssh/id_ed25519）。
resolve_path() {   # $1=值 -> 绝对路径
    path=$1
    case "$path" in
    /*) printf '%s' "$path" ;;
    *) printf '%s/%s' "$PLUGIN_ROOT" "$path" ;;
    esac
}

MNT=$(resolve_path "${SSHFS_MNT:-data/mnt}")
KEY=$(resolve_path "${SSHFS_KEY:-$DATA/ssh/id_ed25519}")

# 插件二进制只认包内路径。
find_bin() {
    [ -x "$HERE/$1" ] || return 1
    printf '%s' "$HERE/$1"
}

# 首次启动生成一套 SSH 密钥；配置里给了 SSHFS_KEY 就用现成的文件。
ensure_key() {
    DROPBEARKEY=$(find_bin dropbearkey) || {
        fail "包里缺少 payload/bin/dropbearkey"
        return 1
    }
    keydir=$(dirname "$KEY")
    mkdir -p "$keydir" 2>/dev/null || {
        fail "建不了密钥目录 $keydir"
        return 1
    }
    chmod 700 "$keydir" 2>/dev/null || true

    if [ ! -s "$KEY" ]; then
        rm -f "$KEY"
        if ! "$DROPBEARKEY" -t ed25519 -f "$KEY" >/dev/null 2>&1 || [ ! -s "$KEY" ]; then
            rm -f "$KEY"
            fail "生成 SSH 私钥失败（$DROPBEARKEY -t ed25519 -f $KEY）"
            return 1
        fi
        chmod 600 "$KEY" 2>/dev/null || true
        log "首次启动：已生成 SSH 私钥 $KEY"
    fi

    if [ ! -s "$KEY.pub" ]; then
        "$DROPBEARKEY" -y -f "$KEY" 2>/dev/null |
            awk '/^ssh-ed25519 / { print; exit }' > "$KEY.pub" 2>/dev/null || true
        chmod 644 "$KEY.pub" 2>/dev/null || true
    fi
    if [ ! -s "$KEY.pub" ]; then
        log "警告：没能导出 $KEY.pub，请用 $DROPBEARKEY -y -f $KEY 手动导出并加到远端用户的 authorized_keys"
    else
        log "SSH 公钥：$KEY.pub（把它加到远端用户的 ~/.ssh/authorized_keys）"
    fi
    return 0
}

SSHFS=""
DBCLIENT=""
FUSERMOUNT=""

mounted() {
    # /proc/mounts 最可靠；没有 /proc 时（手跑测试）退回 mount 的输出。
    if [ -r /proc/mounts ]; then
        awk -v m="$MNT" '$2 == m { found = 1 } END { exit !found }' /proc/mounts
        return
    fi
    mount 2>/dev/null | grep -F " on $MNT " >/dev/null 2>&1
}

unmount_mnt() {
    mounted || return 0
    FUSERMOUNT=$(find_bin fusermount3 2>/dev/null) || FUSERMOUNT=""
    if [ -n "$FUSERMOUNT" ]; then
        "$FUSERMOUNT" -u "$MNT" 2>/dev/null || true
    fi
    mounted || return 0
    umount "$MNT" 2>/dev/null || true
    mounted || return 0
    # 连接已断的失效挂载（Transport endpoint is not connected）只能懒卸载
    umount -l "$MNT" 2>/dev/null || true
    ! mounted
}

do_start() {
    SSHFS=$(find_bin sshfs) || {
        fail "找不到包内的 payload/bin/sshfs"
        return 1
    }
    DBCLIENT=$(find_bin dbclient) || {
        fail "找不到包内的 payload/bin/dbclient"
        return 1
    }
    # 首次启动生成一套 SSH 密钥；SSHFS_KEY 指向的现有文件优先。
    ensure_key || return 1

    if [ ! -d "$MNT" ] && ! mkdir -p "$MNT" 2>/dev/null; then
        fail "建不了挂载点 $MNT"
        return 1
    fi

    if mounted; then
        # 上一次启动留下的挂载（宿主的 stop 没跑完、或手动跑过）：
        # 不先卸掉就再挂一次，两个 sshfs 会互相打架。
        log "$MNT 已经挂着，先卸载残留"
        if ! unmount_mnt; then
            fail "$MNT 卸不掉，拒绝重复挂载"
            return 1
        fi
    fi

    log "挂载 $NUSER@$HOST:$RPATH -> $MNT"
    # dbclient 不认 -o Key=Value（那是 OpenSSH 语法），它的参数只能通过
    # ssh_command 传：-i 私钥、-y 自动接受主机指纹、-p 端口。
    exec "$SSHFS" -o "ssh_command=$DBCLIENT -i $KEY -y -p $PORT" \
        -o reconnect -o "ServerAliveInterval=$ALIVE" \
        "$NUSER@$HOST:$RPATH" "$MNT"
}

do_stop() {
    # 宿主已经终止了 sshfs，这里只收尾；强杀之后挂载点依然在，必须卸载。
    if ! mounted; then
        log "$MNT 未挂载，无需卸载"
        return 0
    fi
    if unmount_mnt; then
        log "已卸载 $MNT"
    else
        log "$MNT 还卸不掉：检查有没有进程占用它（fuser -m $MNT）"
    fi
    return 0
}

json_str() {
    printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g'
}

do_status() {
    if mounted; then
        printf '{"mounted":true,"mountpoint":"%s","host":"%s","path":"%s"}\n' \
            "$(json_str "$MNT")" "$(json_str "$HOST")" "$(json_str "$RPATH")"
    else
        printf '{"mounted":false,"mountpoint":"%s","host":"%s"}\n' \
            "$(json_str "$MNT")" "$(json_str "$HOST")"
    fi
}

case "${1:-}" in
start) do_start ;;
stop) do_stop ;;
status) do_status ;;
*)
    echo "usage: $0 start|stop|status" >&2
    exit 2
    ;;
esac
