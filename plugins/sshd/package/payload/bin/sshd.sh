#!/bin/sh
# sshd：dropbear + sftp-server 的生命周期包装。
#
# 宿主把 lifecycle.start 当成常驻前台进程，所以 start 必须 exec 到 dropbear，
# 不能像旧版那样 start 完就 exit（那样宿主只看到一个已经退出的进程）。
# stop / status 由宿主按 manifest.lifecycle 带参数调用，执行完即退出。
set -u

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
# 宿主给插件的三个目录之一是 SVCD_PLUGIN_DIR（安装目录）。手跑时没有它，就按脚本
# 自己的位置往上三层推：<插件>/<版本>/payload/bin/sshd.sh。
PLUGIN_ROOT=$(CDPATH= cd -- "${SVCD_PLUGIN_DIR:-$HERE/../../..}" && pwd)
# 插件的数据只能落在插件目录里：<插件目录>/data。宿主升级、卸载（不带 --purge）都
# 保留它，dropbear 的主机密钥因此不会变。
DATA="$PLUGIN_ROOT/data"

MULTI="$HERE/sshd"
SFTP_BIN="$HERE/sftp-server"

PORT=2222
CONFIG="$PLUGIN_ROOT/config"
if [ -f "$CONFIG" ]; then
    configured=$(sed -n 's/^[[:space:]]*port[[:space:]]*[:=][[:space:]]*\([0-9][0-9]*\).*/\1/p' "$CONFIG" | head -n 1)
    [ -n "$configured" ] && PORT=$configured
fi

PIDFILE="$DATA/dropbear.pid"
KEY="$DATA/dropbear_host_key"

fail() { echo "sshd: $*" >&2; return 1; }

# 1.0 把数据放在插件目录之外，两种老位置：默认根布局下是 <data>/sshd（也就是插件
# 目录的上上级同名目录），手跑时是 <插件目录>/.data/sshd。第一次 start 搬进来。
migrate_legacy() {
    [ -d "$DATA" ] && return 0
    for old in "$PLUGIN_ROOT/../../sshd" "$PLUGIN_ROOT/.data/sshd"; do
        [ -d "$old" ] || continue
        if mv "$old" "$DATA" 2>/dev/null; then
            echo "sshd: 旧数据 $old 已搬到 $DATA" >&2
        fi
        return 0
    done
    return 0
}

link_applets() {
    mkdir -p "$DATA" || return 1
    ln -sf "$MULTI" "$DATA/dropbear" 2>/dev/null || return 1
    ln -sf "$MULTI" "$DATA/dropbearkey" 2>/dev/null || return 1
}

running() {
    [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE" 2>/dev/null)" 2>/dev/null
}

do_start() {
    migrate_legacy
    link_applets || fail "不能创建 dropbear/dropbearkey 符号链接"
    [ -x "$SFTP_BIN" ] || fail "缺少 $SFTP_BIN"
    if running; then
        echo "already running (pid $(cat "$PIDFILE"), port $PORT)"
        # lifecycle.start 是宿主管理的前台进程；已经有人占着端口时不要假装成功。
        return 1
    fi
    if [ ! -f "$KEY" ]; then
        "$DATA/dropbearkey" -t ed25519 -f "$KEY" >/dev/null 2>&1 || fail "生成主机密钥失败"
    fi
    echo $$ > "$PIDFILE"
    echo "sshd starting on port $PORT" >&2
    # dropbear 的 sftp 子系统路径由补丁优先读这个环境变量；值来自包内相对路径。
    SVCD_SFTP_SERVER="$SFTP_BIN"
    export SVCD_SFTP_SERVER
    # -F 前台运行，-P 写 pid（dropbear 退出时会清理），-r 主机密钥。
    exec "$DATA/dropbear" -F -p "$PORT" -r "$KEY" -P "$PIDFILE"
}

do_stop() {
    if running; then
        pid=$(cat "$PIDFILE")
        kill "$pid" 2>/dev/null || true
        n=0
        while kill -0 "$pid" 2>/dev/null && [ "$n" -lt 5 ]; do
            n=$((n + 1))
            sleep 1
        done
        kill -9 "$pid" 2>/dev/null || true
        rm -f "$PIDFILE"
        echo "stopped (pid $pid)"
    else
        rm -f "$PIDFILE"
        echo "not running"
    fi
}

do_status() {
    if running; then
        printf '{"running":true,"pid":%s,"port":%s}\n' "$(cat "$PIDFILE")" "$PORT"
    else
        printf '{"running":false,"port":%s}\n' "$PORT"
    fi
}

case "${1:-}" in
start)  do_start ;;
stop)   do_stop ;;
status) do_status ;;
*)      echo "usage: $0 start|stop|status" >&2; exit 2 ;;
esac
