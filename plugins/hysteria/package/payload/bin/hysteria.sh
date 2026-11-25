#!/bin/sh
# hysteria：用 mini-sb-agent（sing-box 内核）跑一个 Hysteria2 服务端。
#
# 宿主把 lifecycle.start 当成常驻前台进程，所以 start 直接 exec 内核；
# stop/status 执行后退出。TLS 证书和密码在首次启动时生成到数据目录。
set -u

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
# 宿主给插件的三个目录之一是 SVCD_PLUGIN_DIR（安装目录）。手跑时没有它，就按脚本
# 自己的位置往上三层推（<插件>/<版本>/payload/bin/hysteria.sh）。
PLUGIN_ROOT=$(CDPATH= cd -- "${SVCD_PLUGIN_DIR:-$HERE/../../..}" && pwd)
# 插件的数据只能落在插件目录里：<插件目录>/data。宿主升级、卸载（不带 --purge）都
# 保留它，证书、私钥、密码不会重生成。
DATA="$PLUGIN_ROOT/data"
PIDFILE="$DATA/hysteria.pid"
CONFIG_DST="$DATA/config.json"
CERT="$DATA/cert.pem"
KEY="$DATA/key.pem"
PASSWORD_FILE="$DATA/password"

# 1.0 把数据放在插件目录之外，两种老位置：默认根布局下是 <data>/hysteria（插件目录的
# 上上级同名目录），手跑时是 <插件目录>/.data/hysteria。第一次 start 搬进来：证书和
# 密码跟着走，客户端不用重新配。
migrate_legacy() {
    [ -d "$DATA" ] && return 0
    for old in "$PLUGIN_ROOT/../../hysteria" "$PLUGIN_ROOT/.data/hysteria"; do
        [ -d "$old" ] || continue
        if mv "$old" "$DATA" 2>/dev/null; then
            echo "hysteria: 旧数据 $old 已搬到 $DATA" >&2
        fi
        return 0
    done
    return 0
}

find_config() {
    for candidate in "${SVCD_CONFIG:-}" "$PLUGIN_ROOT/config" "$PLUGIN_ROOT/package/config/config"; do
        [ -n "$candidate" ] && [ -f "$candidate" ] && { printf '%s' "$candidate"; return 0; }
    done
    return 1
}

password() {
    if [ -s "$PASSWORD_FILE" ]; then
        cat "$PASSWORD_FILE"
    else
        if command -v openssl >/dev/null 2>&1; then
            openssl rand -hex 16 > "$PASSWORD_FILE"
        else
            od -An -N16 -tx1 /dev/urandom | tr -d ' \n' > "$PASSWORD_FILE"
        fi
        cat "$PASSWORD_FILE"
    fi
}

ensure_cert() {
    [ -s "$CERT" ] && [ -s "$KEY" ] && return 0
    command -v openssl >/dev/null 2>&1 || {
        echo 'hysteria: openssl is required to generate a self-signed TLS certificate' >&2
        return 1
    }
    openssl req -x509 -newkey rsa:2048 -sha256 -days 3650 -nodes \
        -keyout "$KEY" -out "$CERT" -subj '/CN=svcd-hysteria' >/dev/null 2>&1 || return 1
    chmod 600 "$KEY" 2>/dev/null || true
    return 0
}

prepare_config() {
    mkdir -p "$DATA" 2>/dev/null || { echo "hysteria: 建不了数据目录 $DATA" >&2; return 1; }
    src=$(find_config) || { echo 'hysteria: config.json missing' >&2; return 1; }
    # 只有模板里的占位符才替换；用户自己写死证书路径/密码时原样保留。
    secret=$(password) || return 1
    if grep -q -e '@CERT@' -e '@KEY@' "$src" 2>/dev/null; then
        ensure_cert || return 1
    fi
    sed -e "s#@PASSWORD@#$secret#g" -e "s#@CERT@#$CERT#g" -e "s#@KEY@#$KEY#g" \
        "$src" > "$CONFIG_DST" || return 1
    printf '%s' "$CONFIG_DST"
}

running() {
    [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE" 2>/dev/null)" 2>/dev/null
}

do_start() {
    migrate_legacy
    cfg=$(prepare_config) || return 1
    if running; then
        echo "already running (pid $(cat "$PIDFILE"), port 8443)" >&2
        return 1
    fi
    echo $$ > "$PIDFILE"
    echo "hysteria starting (config=$cfg)" >&2
    exec "$HERE/mini-sb-agent" -config "$cfg"
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
        printf '{"running":true,"pid":%s,"port":8443}\n' "$(cat "$PIDFILE")"
    else
        printf '{"running":false,"port":8443}\n'
    fi
}

case "${1:-}" in
start)  do_start ;;
stop)   do_stop ;;
status) do_status ;;
*)      echo "usage: $0 start|stop|status" >&2; exit 2 ;;
esac
