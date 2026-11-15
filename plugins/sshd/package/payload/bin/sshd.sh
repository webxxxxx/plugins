#!/bin/sh
# sshd：dropbear + sftp-server 的插件包装。能力：start / stop / status。
set -u
HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
. "$HERE/../lib/svcd-plugin.sh"

DATA=${SVCD_DATA_DIR:-$HERE/.sshd}
mkdir -p "$DATA" 2>/dev/null || true
PORT=$(svcd_config_get port); PORT=${PORT:-22}
MULTI="$HERE/sshd"
PIDFILE="$DATA/dropbear.pid"
KEY="$DATA/dropbear_host_key"

# dropbearmulti 按 argv[0] 分派，需要 dropbear / dropbearkey 这两个名字
link_applets() {
    ln -sf "$MULTI" "$DATA/dropbear" 2>/dev/null || true
    ln -sf "$MULTI" "$DATA/dropbearkey" 2>/dev/null || true
}

running() {
    [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE" 2>/dev/null)" 2>/dev/null
}

# dropbear 只认 /usr/libexec/sftp-server；包里的副本在需要时放到那儿
deploy_sftp() {
    [ -x /usr/libexec/sftp-server ] && return 0
    mkdir -p /usr/libexec 2>/dev/null || true
    cp "$HERE/sftp-server" /usr/libexec/sftp-server 2>/dev/null || true
    chmod 755 /usr/libexec/sftp-server 2>/dev/null || true
}

do_start() {
    link_applets
    deploy_sftp
    if running; then
        printf 'already running (pid %s, port %s)' "$(cat "$PIDFILE")" "$PORT"
        return 0
    fi
    [ -f "$KEY" ] || "$DATA/dropbearkey" -t ed25519 -f "$KEY" >/dev/null 2>&1 || true
    "$DATA/dropbear" -p "$PORT" -r "$KEY" -P "$PIDFILE" >/dev/null 2>&1 || true
    sleep 1
    if running; then
        printf 'started (pid %s, port %s)' "$(cat "$PIDFILE")" "$PORT"
    else
        echo 'start failed: dropbear did not stay up' >&2
        return 1
    fi
}

do_stop() {
    if running; then
        kill "$(cat "$PIDFILE")" 2>/dev/null || true
        rm -f "$PIDFILE"
        printf 'stopped'
    else
        rm -f "$PIDFILE"
        printf 'not running'
    fi
}

do_status() {
    if running; then
        printf '{"running":true,"pid":%s,"port":%s}' "$(cat "$PIDFILE")" "$PORT"
    else
        printf '{"running":false,"port":%s}' "$PORT"
    fi
}

handle() {
    case "$1" in
    start) do_start ;;
    stop) do_stop ;;
    status) do_status ;;
    *) echo "unknown tool: $1" >&2; return 1 ;;
    esac
}

# 插件启动即拉起守护进程；日志走 stderr，不污染协议 stdout
do_start >&2 || true

svcd_plugin_serve handle
