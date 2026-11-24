#!/bin/sh
# meow-fw.sh — 网关形态（proxy-mode: router）的 nat 规则：up | down | status | check
#
# 为什么不用 meow 内置的托管防火墙：路由器内核 `# CONFIG_NF_TABLES is not set`，
# 也没有 nft 命令，meow 在 listener 启动阶段就会失败。所以网关配置里的 tproxy
# 入口写 firewall: false，规则由本脚本用 iptables 成对管理：
#   start : 起 meow → 等 tproxy 端口监听 → up
#   stop  : down → 停 meow
#   退出  : down（宁可直接连，也别让 LAN 一直打到死端口）
#
# 为什么不用 `-m addrtype --dst-type LOCAL` 放行本机地址：厂商内核
# `# CONFIG_NETFILTER_XT_MATCH_ADDRTYPE is not set`，写了也加载不了；
# 网关的 LAN IP 一定落在私有网段里，靠下面的网段放行就够。
# 不要加 198.18.0.0/16：fake-ip 必须被劫持。
#
# 用法：meow-fw.sh up|down|status|check
#          [--lan br0] [--tproxy 7893] [--dns 1053] [--mark 9527]
#       --dns / --mark 省略时，对应的 DNS 劫持 / SO_MARK 放行规则不装。
set -u

LAN_IF=br0
TPROXY_PORT=7893
DNS_PORT=""
MARK=""
IPT="iptables -t nat"

log() { echo "[meow-fw] $*" >&2; }
die() { echo "meow-fw: $*" >&2; exit 1; }

is_port() {
    case "${1:-}" in
        ''|*[!0-9]*) return 1 ;;
    esac
    [ "$1" -ge 1 ] && [ "$1" -le 65535 ]
}

need_ipt() {
    command -v iptables >/dev/null 2>&1 || die "iptables not found"
    $IPT -L -n >/dev/null 2>&1 || die "iptables nat table unavailable"
}

# 摘掉 PREROUTING 里所有跳到自己链的规则。不依赖 --lan：改过网口也能清干净；
# 老版本 iptables 没有 -S 时退回按当前网口精确删除。
detach() {
    _found=0
    while IFS= read -r _line; do
        case "$_line" in
            '-A PREROUTING '*) ;;
            *) continue ;;
        esac
        case "$_line" in
            *' -j MEOW_DNS'|*' -j MEOW_TPROXY') ;;
            *) continue ;;
        esac
        _found=1
        # 我们自己的规则里没有空格和引号，按空白拆开就能原样回放给 -D。
        $IPT -D PREROUTING ${_line#-A PREROUTING } 2>/dev/null
    done <<EOF
$($IPT -S PREROUTING 2>/dev/null)
EOF
    if [ "$_found" = 0 ]; then
        while $IPT -D PREROUTING -i "$LAN_IF" -j MEOW_DNS 2>/dev/null; do :; done
        while $IPT -D PREROUTING -i "$LAN_IF" -j MEOW_TPROXY 2>/dev/null; do :; done
    fi
}

chains_up() {
    for c in MEOW_DNS MEOW_TPROXY; do
        $IPT -L "$c" -n >/dev/null 2>&1 || $IPT -N "$c" || die "cannot create chain $c"
        $IPT -F "$c" || die "cannot flush chain $c"
    done
}

up() {
    need_ipt
    [ -e "/sys/class/net/$LAN_IF" ] || die "interface $LAN_IF not found (use --lan)"
    is_port "$TPROXY_PORT" || die "bad --tproxy '${TPROXY_PORT}'"
    if [ -n "$DNS_PORT" ] && ! is_port "$DNS_PORT"; then
        die "bad --dns '$DNS_PORT'"
    fi
    if [ -n "$MARK" ]; then
        case "$MARK" in
            *[!0-9]*) die "bad --mark '$MARK'" ;;
        esac
    fi

    echo 1 > /proc/sys/net/ipv4/ip_forward 2>/dev/null \
        || log "warn: cannot enable ip_forward"

    chains_up

    # 1) DNS 劫持：REDIRECT 落到入接口主地址，等价官方 dnat 到 LAN_IP
    if [ -n "$DNS_PORT" ]; then
        $IPT -A MEOW_DNS -p udp --dport 53 -j REDIRECT --to-ports "$DNS_PORT" \
            || die "dns/udp rule failed"
        $IPT -A MEOW_DNS -p tcp --dport 53 -j REDIRECT --to-ports "$DNS_PORT" \
            || die "dns/tcp rule failed"
    fi

    # 2) meow 自己的出向（SO_MARK=routing-mark）免回环
    if [ -n "$MARK" ]; then
        $IPT -A MEOW_TPROXY -m mark --mark "$MARK" -j RETURN \
            || die "mark bypass rule failed"
    fi

    # 3) 保留网段放行
    for r in 0.0.0.0/8 10.0.0.0/8 100.64.0.0/10 127.0.0.0/8 169.254.0.0/16 \
             172.16.0.0/12 192.168.0.0/16 224.0.0.0/4 240.0.0.0/4; do
        $IPT -A MEOW_TPROXY -d "$r" -j RETURN || die "bypass rule $r failed"
    done

    # 4) 其余 TCP 全部劫持
    $IPT -A MEOW_TPROXY -p tcp -j REDIRECT --to-ports "$TPROXY_PORT" \
        || die "tcp redirect rule failed"

    # 跳转：先摘旧的，再固定顺序插到最前面（DNS 在 tproxy 之前）
    detach
    if [ -n "$DNS_PORT" ]; then
        $IPT -I PREROUTING 1 -i "$LAN_IF" -j MEOW_DNS || die "cannot hook MEOW_DNS"
        $IPT -I PREROUTING 2 -i "$LAN_IF" -j MEOW_TPROXY || die "cannot hook MEOW_TPROXY"
    else
        $IPT -I PREROUTING 1 -i "$LAN_IF" -j MEOW_TPROXY || die "cannot hook MEOW_TPROXY"
    fi

    log "up lan=$LAN_IF tproxy=:$TPROXY_PORT dns=:${DNS_PORT:-off} mark=${MARK:-off}"
    return 0
}

down() {
    need_ipt
    detach
    for c in MEOW_DNS MEOW_TPROXY; do
        $IPT -F "$c" 2>/dev/null
        $IPT -X "$c" 2>/dev/null
    done
    log "down (rules removed, traffic goes direct)"
    return 0
}

check() {
    command -v iptables >/dev/null 2>&1 || return 1
    $IPT -C PREROUTING -i "$LAN_IF" -j MEOW_TPROXY >/dev/null 2>&1 || return 1
    if [ -n "$DNS_PORT" ]; then
        $IPT -C PREROUTING -i "$LAN_IF" -j MEOW_DNS >/dev/null 2>&1 || return 1
    fi
    return 0
}

status() {
    echo "ip_forward : $(cat /proc/sys/net/ipv4/ip_forward 2>/dev/null)"
    echo "lan        : $LAN_IF  tproxy :$TPROXY_PORT  dns :${DNS_PORT:-off}  mark ${MARK:-off}"
    if check; then
        echo "hooks      : meow nat chains are hooked"
    else
        echo "hooks      : MISSING (meow traffic is not intercepted)"
    fi
    echo "--- PREROUTING ---"
    $IPT -L PREROUTING -n -v --line-numbers 2>/dev/null | sed -n '1,10p'
    echo "--- MEOW_DNS ---"
    $IPT -L MEOW_DNS -n -v 2>/dev/null | sed -n '1,8p'
    echo "--- MEOW_TPROXY ---"
    $IPT -L MEOW_TPROXY -n -v 2>/dev/null | sed -n '1,16p'
}

case "${1:-}" in
    up|down|status|check) _cmd=$1; shift ;;
    -h|--help) sed -n '2,18p' "$0"; exit 0 ;;
    *) echo "usage: $0 up|down|status|check [--lan IF] [--tproxy PORT] [--dns PORT] [--mark N]" >&2; exit 2 ;;
esac

while [ $# -gt 0 ]; do
    case "$1" in
        --lan|--tproxy|--dns|--mark)
            [ $# -ge 2 ] || die "missing value for $1"
            case "$1" in
                --lan)    LAN_IF=$2 ;;
                --tproxy) TPROXY_PORT=$2 ;;
                --dns)    DNS_PORT=$2 ;;
                --mark)   MARK=$2 ;;
            esac
            shift 2 ;;
        *) die "unknown option: $1" ;;
    esac
done

case "$_cmd" in
    up)     up ;;
    down)   down ;;
    status) status ;;
    check)  check ;;
esac
