#!/bin/sh
# wireguardd: the supervised process for the wireguard plugin.
#
# 它同时做三件事：
#   1. 启动时把隧道拉起来（wireguard.sh up）——必须是本进程的子进程，宿主
#      停服按进程组 SIGTERM 才能把 gotatun 一起收走；
#   2. 在 stdout 上讲协议 1（JSON-RPC），给管理页面提供 state / save_settings /
#      add_peer / remove_peer / peer_config；
#   3. 在后台看守：内核后端跑 wg-lan-fix.sh 自愈循环，用户态后端看 gotatun
#      是否还活着，死了就重建。
#
# 设置和 peer 都存在插件目录里（data/settings、data/peers.tsv）；没有单独的
# 配置文件，所有可改项都在管理页面。
set -u

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
. "$HERE/../lib/svcd-plugin.sh"
. "$HERE/../lib/wireguard-lib.sh"

load_settings || exit 1
migrate_legacy_peer || true

json_str() { printf '%s' "$1" | svcd_json_escape; }

is_uint() {
    case "$1" in
    '' | *[!0-9]*) return 1 ;;
    esac
    [ "$1" -ge "$2" ] 2>/dev/null && [ "$1" -le "$3" ] 2>/dev/null
}

is_iface() {
    case "$1" in
    '' | *[!A-Za-z0-9_.:-]*) return 1 ;;
    esac
    [ ${#1} -le 15 ]
}

is_ipv4() {
    printf '%s\n' "$1" | awk -F '.' '
        NF == 4 {
            for (i = 1; i <= 4; i++)
                if ($i !~ /^[0-9]+$/ || $i + 0 < 0 || $i + 0 > 255) exit 1
            exit 0
        }
        { exit 1 }'
}

is_net24() {
    case "$1" in
    */24) ;;
    *) return 1 ;;
    esac
    base=${1%/24}
    is_ipv4 "$base" || return 1
    last=$(printf '%s' "$base" | awk -F '.' '{ print $4 }')
    [ "$last" = 0 ]
}

last_octet() { printf '%s' "$1" | awk -F '.' '{ print $4 }'; }

check_settings() {
    is_iface "$WG_LAN_IF" || {
        fail "LAN 接口不合法：$WG_LAN_IF"
        return 1
    }
    is_uint "$WG_PORT" 1 65535 || {
        fail "端口不合法：$WG_PORT"
        return 1
    }
    is_net24 "$WG_NET" || {
        fail "隧道网段目前只支持 /24（例如 10.99.0.0/24）：$WG_NET"
        return 1
    }
    is_ipv4 "$WG_ADDR" || {
        fail "服务端隧道地址不合法：$WG_ADDR"
        return 1
    }
    _netbase=${WG_NET%/24}
    [ "$(printf '%s' "$_netbase" | cut -d. -f1-3)" = "$(printf '%s' "$WG_ADDR" | cut -d. -f1-3)" ] || {
        fail "服务端地址 $WG_ADDR 不在网段 $WG_NET 内"
        return 1
    }
    _addr_last=$(last_octet "$WG_ADDR")
    [ "$_addr_last" -ge 1 ] && [ "$_addr_last" -le 254 ] || {
        fail "服务端地址不能是网络地址/广播地址：$WG_ADDR"
        return 1
    }
    is_uint "$WG_MTU" 576 1500 || {
        fail "MTU 不合法（576-1500）：$WG_MTU"
        return 1
    }
    is_uint "$WG_TABLE" 1 65535 || {
        fail "路由表号不合法：$WG_TABLE"
        return 1
    }
    is_uint "$WG_KEEPALIVE" 0 65535 || {
        fail "PersistentKeepalive 不合法：$WG_KEEPALIVE"
        return 1
    }
    case "$WG_ENDPOINT" in
    *[!A-Za-z0-9_.:\[\]-]*)
        fail "客户端连接地址不合法：$WG_ENDPOINT"
        return 1
        ;;
    esac
    case "$WG_DNS" in
    *[!0-9A-Fa-f:.,]*)
        fail "DNS 不合法（逗号分隔的 IP）：$WG_DNS"
        return 1
        ;;
    esac
    case "$WG_ALLOWED_IPS" in
    '' | *[!0-9A-Fa-f:./,]*)
        fail "AllowedIPs 不合法（逗号分隔的网段）：$WG_ALLOWED_IPS"
        return 1
        ;;
    esac
    return 0
}

guardian_running() {
    [ -n "${GUARD_WATCH:-}" ] && kill -0 "$GUARD_WATCH" 2>/dev/null
}

active_peers() {
    "$WG_TOOL" show "$WG_IF" peers 2>/dev/null | wc -l | tr -d ' '
}

settings_json() {
    printf '{"lan_interface":"%s","port":%s,"network":"%s","address":"%s","mtu":%s,"table":%s,"endpoint":"%s","dns":"%s","allowed_ips":"%s","keepalive":%s}' \
        "$(json_str "$WG_LAN_IF")" "$WG_PORT" "$(json_str "$WG_NET")" \
        "$(json_str "$WG_ADDR")" "$WG_MTU" "$WG_TABLE" "$(json_str "$WG_ENDPOINT")" \
        "$(json_str "$WG_DNS")" "$(json_str "$WG_ALLOWED_IPS")" "$WG_KEEPALIVE"
}

peers_json() {
    printf '['
    _first=1
    if [ -f "$PEERS" ]; then
        while IFS="$(printf '\t')" read -r id name addr pub priv created; do
            [ -n "$id" ] || continue
            [ "$_first" = 1 ] || printf ','
            _first=0
            printf '{"id":%s,"name":"%s","address":"%s","public_key":"%s","created":"%s"}' \
                "$id" "$(json_str "$name")" "$(json_str "$addr")" \
                "$(json_str "$pub")" "$(json_str "$created")"
        done < "$PEERS"
    fi
    printf ']'
}

status_json() {
    _up=false
    if ip link show "$WG_IF" >/dev/null 2>&1 &&
        ip link show "$WG_IF" 2>/dev/null | grep -q 'UP'; then
        _up=true
    fi
    _guard=false
    guardian_running && _guard=true
    _active=0
    [ "$_up" = true ] && _active=$(active_peers)
    printf '{"interface_up":%s,"impl":"%s","guardian":%s,"active_peers":%s,"listen_port":%s}' \
        "$_up" "$(wg_impl)" "$_guard" "$_active" "$WG_PORT"
}

state() {
    printf '{"settings":'
    settings_json
    printf ',"server_public_key":"%s","peers":' \
        "$(json_str "$(cat "$SERVER_PUB" 2>/dev/null)")"
    peers_json
    printf ',"status":'
    status_json
    printf '}'
}

save_settings() {
    old_lan=$WG_LAN_IF
    old_port=$WG_PORT
    old_net=$WG_NET
    old_addr=$WG_ADDR
    old_mtu=$WG_MTU
    old_table=$WG_TABLE

    WG_LAN_IF=$(svcd_field "$1" lan_interface)
    WG_PORT=$(svcd_raw_field "$1" port)
    WG_NET=$(svcd_field "$1" network)
    WG_ADDR=$(svcd_field "$1" address)
    WG_MTU=$(svcd_raw_field "$1" mtu)
    WG_TABLE=$(svcd_raw_field "$1" table)
    WG_ENDPOINT=$(svcd_field "$1" endpoint)
    WG_DNS=$(svcd_field "$1" dns)
    WG_ALLOWED_IPS=$(svcd_field "$1" allowed_ips)
    WG_KEEPALIVE=$(svcd_raw_field "$1" keepalive)
    case "$WG_NET" in
    */*) ;;
    *) WG_NET="$WG_NET/24" ;;
    esac
    [ -n "$WG_ALLOWED_IPS" ] || WG_ALLOWED_IPS='0.0.0.0/0,::/0'
    PREFIX=${WG_NET##*/}

    if ! check_settings; then
        load_settings || true
        return 1
    fi

    # peer 地址是按旧网段分配的：改网段会让现有配置全部失效。
    if [ -s "$PEERS" ]; then
        if [ "$old_net" != "$WG_NET" ]; then
            fail "已有 peer，不能改隧道网段；先删除 peer 再改"
            load_settings || true
            return 1
        fi
        if [ "$old_addr" != "$WG_ADDR" ] && [ -f "$PEERS" ] &&
            awk -F '\t' -v a="$WG_ADDR" '$3 == a { found = 1 } END { exit !found }' "$PEERS"; then
            fail "服务端地址 $WG_ADDR 已被某个 peer 占用"
            load_settings || true
            return 1
        fi
    fi

    restart=0
    [ "$old_lan" = "$WG_LAN_IF" ] || restart=1
    [ "$old_port" = "$WG_PORT" ] || restart=1
    [ "$old_net" = "$WG_NET" ] || restart=1
    [ "$old_addr" = "$WG_ADDR" ] || restart=1
    [ "$old_mtu" = "$WG_MTU" ] || restart=1

    cp "$SETTINGS" "$SETTINGS.bak" 2>/dev/null || true
    if [ "$restart" = 1 ]; then
        "$HERE/wireguard.sh" stop >/dev/null 2>&1 || true
    fi
    if ! write_settings; then
        [ -f "$SETTINGS.bak" ] && mv "$SETTINGS.bak" "$SETTINGS"
        load_settings || true
        [ "$restart" = 1 ] && WG_SKIP_WAIT=1 "$HERE/wireguard.sh" start >/dev/null 2>&1 || true
        return 1
    fi
    if ! load_settings; then
        [ -f "$SETTINGS.bak" ] && mv "$SETTINGS.bak" "$SETTINGS"
        load_settings || true
        [ "$restart" = 1 ] && WG_SKIP_WAIT=1 "$HERE/wireguard.sh" start >/dev/null 2>&1 || true
        return 1
    fi

    if [ "$restart" = 1 ] || [ "$old_lan" != "$WG_LAN_IF" ] || [ "$old_table" != "$WG_TABLE" ]; then
        pkill -f "$HERE/wg-lan-fix.sh" 2>/dev/null || true
    fi
    if [ "$restart" = 1 ]; then
        if ! WG_SKIP_WAIT=1 "$HERE/wireguard.sh" start >/dev/null 2>&1; then
            [ -f "$SETTINGS.bak" ] && mv "$SETTINGS.bak" "$SETTINGS"
            load_settings || true
            "$HERE/wireguard.sh" start >/dev/null 2>&1 || true
            fail "设置已回滚：隧道重建失败"
            return 1
        fi
    fi
    rm -f "$SETTINGS.bak"
    printf '{"ok":true,"restarted":%s}' "$([ "$restart" = 1 ] && echo true || echo false)"
}

add_peer() {
    name=$(svcd_field "$1" peer_name)
    name=$(printf '%s' "$name" | tr -d '\t\r\n' | awk '{ print substr($0, 1, 64) }')
    [ -n "$name" ] || name=peer

    [ -n "$WG_ENDPOINT" ] || {
        fail "先在设置页填写并保存“客户端连接地址”"
        return 1
    }
    ensure_keys || return 1
    peers_init || {
        fail "初始化 peer 存储失败"
        return 1
    }
    addr=$(peer_next_addr) || {
        fail "隧道地址已用完（只支持 /24），先删除不用的 peer"
        return 1
    }

    priv=$("$WG_TOOL" genkey 2>/dev/null) || priv=""
    [ -n "$priv" ] || {
        fail "生成客户端私钥失败"
        return 1
    }
    pub=$(printf '%s\n' "$priv" | "$WG_TOOL" pubkey 2>/dev/null) || pub=""
    [ -n "$pub" ] || {
        fail "生成客户端公钥失败"
        return 1
    }
    id=$(peer_next_id)
    created=$(date +%s 2>/dev/null || echo 0)
    printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$id" "$name" "$addr" "$pub" "$priv" "$created" >> "$PEERS"
    chmod 600 "$PEERS" 2>/dev/null || true

    apply_peer "$pub" "$addr" || log "wg0 不在，peer 将在下次启动时应用"
    config=$(peer_config "$id") || {
        fail "peer 已保存，但生成配置失败（检查设置里的连接地址）"
        return 1
    }
    printf '{"ok":true,"peer":{"id":%s,"name":"%s","address":"%s","public_key":"%s","created":"%s"},"config":"%s"}' \
        "$id" "$(json_str "$name")" "$(json_str "$addr")" "$(json_str "$pub")" \
        "$(json_str "$created")" "$(json_str "$config")"
}

remove_peer() {
    id=$(svcd_raw_field "$1" peer_id)
    [ -n "$id" ] || {
        fail "缺少 peer_id"
        return 1
    }
    peer_find "$id" || {
        fail "找不到 peer $id"
        return 1
    }
    remove_peer_from_kernel "$P_PUB" || true
    _tmp="$PEERS.$$"
    awk -F '\t' -v id="$id" '$1 != id' "$PEERS" > "$_tmp" || {
        rm -f "$_tmp"
        fail "更新 peer 列表失败"
        return 1
    }
    chmod 600 "$_tmp" 2>/dev/null || true
    mv "$_tmp" "$PEERS"
    printf '{"ok":true,"id":%s}' "$id"
}

peer_config_text() {
    id=$(svcd_raw_field "$1" peer_id)
    [ -n "$id" ] || {
        fail "缺少 peer_id"
        return 1
    }
    peer_config "$id"
}

# 看守：两个后端只有“怎么保活”不同。
#   kernel    -> 周期跑 wg-lan-fix.sh（厂商固件会刷回 rp_filter / 策略路由）
#   userspace -> gotatun 死了就重建隧道
start_guard() {
    _parent=$$
    (
        while kill -0 "$_parent" 2>/dev/null; do
            case "$(wg_impl)" in
            kernel)
                if ip link show "$WG_IF" >/dev/null 2>&1; then
                    [ "$WG_LAN_FIX" = off ] || "$HERE/wg-lan-fix.sh" >&2 || true
                else
                    # 接口被删 / 模块被卸载：重建。失败时隔久一点再试，别刷屏。
                    WG_SKIP_WAIT=1 "$HERE/wireguard.sh" up >&2 || sleep 25
                fi
                ;;
            userspace)
                if ! gotatun_alive; then
                    log "gotatun 不在了，重建隧道"
                    WG_SKIP_WAIT=1 "$HERE/wireguard.sh" up >&2 || sleep 25
                fi
                ;;
            esac
            sleep 5
        done
    ) &
    GUARD_WATCH=$!
}

svcd_status() {
    printf 'running, peers %s' "$(active_peers 2>/dev/null || echo 0)"
}

handle() {
    case "$1" in
    state) state ;;
    save_settings) save_settings "$2" ;;
    add_peer) add_peer "$2" ;;
    remove_peer) remove_peer "$2" ;;
    peer_config) peer_config_text "$2" ;;
    *)
        fail "unknown tool: $1"
        return 1
        ;;
    esac
}

# 隧道由被监督进程自己拉起来：gotatun 必须是它的子进程，宿主停服按进程组
# SIGTERM 才能一起收走；内核后端走同一条路，两个后端行为一致。
# 先建隧道再起看守：反过来的话看守会和这里的 up 并发建接口。
"$HERE/wireguard.sh" up >&2 || log "隧道启动失败，看守会重试；管理页面也可保存设置触发重建"
start_guard
WIREGUARD_CAPABILITY=1
svcd_plugin_serve handle
