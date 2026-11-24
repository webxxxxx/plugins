#!/bin/sh
# meow：包装 vendor/meow-rs 编译出来的二进制。
#
# 两种运行形态，由活动配置的顶层键 proxy-mode 决定（缺省 pc）：
#
#   pc      本机透明代理：配置里写 tproxy-port，meow 自己装/拆 nft output 链，
#           插件不碰防火墙（需要 root + nftables）。
#   router  网关：插件劫持 LAN 的转发流量，nat 规则由 meow-fw.sh 用 iptables
#           成对管理（需要 root + iptables；内核没有 nf_tables 也能用）。
#
# 宿主把 lifecycle.start 当成常驻前台进程：start 一直不返回，直到 meow 退出；
# stop/status 执行后退出。路径全部从脚本位置推导，数据目录用 SVCD_DATA_DIR。
#
# 注意：meow 写回配置（订阅刷新、面板保存）时会按结构重新序列化，顶层未知键
# 会被删掉。所以 proxy-mode / lan-interface 每次读到的值都镜像到
# $DATA/<键>；配置里没有了就沿用镜像并告警，避免路由器形态被悄悄降级成 pc。
set -u

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "$HERE/.." && pwd)          # payload/：yacd、cn_domains 在这一层
PLUGIN_ROOT=$(CDPATH= cd -- "$HERE/../../.." && pwd)
BASE=${SVCD_DATA_DIR:-$PLUGIN_ROOT/.data}
DATA="$BASE/meow"
PIDFILE="$DATA/meow.pid"
CONFIG="$PLUGIN_ROOT/config"
CFG="$CONFIG"
DATA_CONFIG="$DATA/config.yaml"
FW="$HERE/meow-fw.sh"

mkdir -p "$DATA" 2>/dev/null || true

# 配置直接使用安装后的活动配置，订阅/API 的写回才会出现在控制台里。
# 只有源码 checkout 没有活动配置时，才用包内模板生成一份数据目录副本。
prepare_config() {
    if [ ! -f "$CONFIG" ]; then
        template="$PLUGIN_ROOT/package/config/config"
        [ -f "$template" ] || { echo 'meow: config.yaml missing' >&2; return 1; }
        if [ ! -f "$DATA_CONFIG" ]; then
            sed -e "s#@DATA@#$DATA#g" "$template" > "$DATA_CONFIG" || return 1
        fi
        CONFIG="$DATA_CONFIG"
    elif grep -q '@DATA@' "$CONFIG"; then
        # 旧布局：订阅数据可能已经写进数据目录的副本；先把它迁回活动配置。
        if [ -f "$DATA_CONFIG" ] && grep -q '^subscriptions:' "$DATA_CONFIG"; then
            cp "$DATA_CONFIG" "$CONFIG" || return 1
        fi
        if grep -q '@DATA@' "$CONFIG"; then
            tmp="$CONFIG.tmp.$$"
            sed -e "s#@DATA@#$DATA#g" "$CONFIG" > "$tmp" && mv "$tmp" "$CONFIG" || return 1
        fi
    fi
    printf '%s' "$CONFIG"
}

# 顶层标量：只认行首的 `键: 值`，去掉行尾注释、引号和首尾空白。
cfg_value() {
    sed -n "s/^$1:[[:space:]]*\([^#]*\).*/\1/p" "$CFG" 2>/dev/null \
        | head -n 1 \
        | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' -e 's/^"//' -e 's/"$//'
}

# dns.listen → "地址 端口"；没有或解析不出就什么都不输出。
dns_listen() {
    awk '
    function clean(s) { sub(/[ \t]*#.*$/, "", s); gsub(/^[ \t]+|[ \t]+$/, "", s); gsub(/"/, "", s); return s }
    BEGIN { inblock = 0 }
    /^[^ \t#]/ { inblock = ($0 ~ /^dns:[ \t]*$/); next }
    !inblock { next }
    /^[ \t]+listen:[ \t]*/ {
        v = $0
        sub(/^[ \t]+listen:[ \t]*/, "", v)
        v = clean(v)
        if (v == "") next
        i = match(v, /:[^:]*$/)
        if (i == 0) next
        addr = substr(v, 1, i - 1)
        port = substr(v, i + 1)
        gsub(/^\[|\]$/, "", addr)
        print addr, port
        exit
    }' "$CFG"
}

# dns.enable → true/false；没有就什么都不输出（meow 默认不开 DNS）。
dns_enable() {
    awk '
    BEGIN { inblock = 0 }
    /^[^ \t#]/ { inblock = ($0 ~ /^dns:[ \t]*$/); next }
    !inblock { next }
    /^[ \t]+enable:[ \t]*/ {
        v = $0
        sub(/^[ \t]+enable:[ \t]*/, "", v)
        sub(/[ \t]*#.*$/, "", v)
        gsub(/^[ \t]+|[ \t]+$/, "", v)
        gsub(/"/, "", v)
        print v
        exit
    }' "$CFG"
}

# listeners: 里第一个 type: tproxy 的条目 → "端口 firewall listen"。
# 只支持块写法（- name: … 换行 port: …），一行流式 map 不认。
tproxy_entry() {
    awk '
    function clean(s) { sub(/[ \t]*#.*$/, "", s); gsub(/^[ \t]+|[ \t]+$/, "", s); gsub(/"/, "", s); return s }
    function value(line, key,   s) {
        s = line
        sub(/^[ \t]*/, "", s)
        sub(/^-[ \t]*/, "", s)
        if (index(s, key ":") != 1) return ""
        sub("^" key ":[ \t]*", "", s)
        return clean(s)
    }
    BEGIN { inblock = 0; done = 0; type = ""; port = ""; fw = ""; listen = "" }
    /^[^ \t#]/ { inblock = ($0 ~ /^listeners:[ \t]*$/); next }
    !inblock { next }
    /^[ \t]*-[ \t]*/ {
        if (type == "tproxy") { print port, fw, listen; done = 1; exit }
        type = ""; port = ""; fw = ""; listen = ""
    }
    { v = value($0, "type");     if (v != "") type = v }
    { v = value($0, "port");     if (v != "") port = v }
    { v = value($0, "firewall"); if (v != "") fw = v }
    { v = value($0, "listen");   if (v != "") listen = v }
    END { if (!done && type == "tproxy") print port, fw, listen }
    ' "$CFG"
}

# 读一个插件级设置：配置里优先；没有了就用数据目录镜像（并告警），最后才用缺省值。
# $3 非空表示安静模式（status 轮询用，不要每次打告警）。
setting() {
    _key=$1
    _v=$(cfg_value "$_key")
    if [ -z "$_v" ] || [ "$_v" = null ]; then
        if [ -f "$DATA/$_key" ]; then
            _v=$(cat "$DATA/$_key" 2>/dev/null)
            if [ -z "${3:-}" ]; then
                echo "meow: 配置里没有 $_key（meow 写回配置时会丢掉未知键），沿用上次的 '$_v'" >&2
            fi
        else
            _v=$2
        fi
    fi
    # 安静模式（status 轮询）只读不写，避免每个轮询都落盘。
    if [ -n "$_v" ] && [ -z "${3:-}" ]; then
        printf '%s\n' "$_v" > "$DATA/$_key" 2>/dev/null || true
    fi
    printf '%s' "$_v"
}

resolve_mode() {
    _mode=$(setting proxy-mode pc "${1:-}")
    case "$_mode" in
        pc|router) printf '%s' "$_mode" ;;
        *) echo "meow: proxy-mode 只能是 pc 或 router（现在是 '$_mode'）" >&2; return 1 ;;
    esac
}

is_port() {
    case "${1:-}" in
        ''|*[!0-9]*) return 1 ;;
    esac
    [ "$1" -ge 1 ] && [ "$1" -le 65535 ]
}

running() {
    [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE" 2>/dev/null)" 2>/dev/null
}

# /proc/net/tcp 里本地端口是十六进制，状态 0A = LISTEN（busybox 不一定有 netstat）。
# 网关形态要求 listener 绑 0.0.0.0，所以只看 v4 表。
port_listening() {
    _hex=$(printf '%04X' "$1" 2>/dev/null) || return 1
    grep -qE "^ *[0-9]+: [0-9A-F]+:$_hex [0-9A-F]+:[0-9A-F]+ 0A" /proc/net/tcp 2>/dev/null
}

report_ports() {
    cfg="$1"
    [ -f "$cfg" ] || return 0
    for key in mixed-port port socks-port; do
        value=$(sed -n "s/^${key}:[[:space:]]*\([0-9][0-9]*\).*/\1/p" "$cfg" | head -n 1)
        [ -n "$value" ] && printf ',"%s":%s' "$key" "$value"
    done
    value=$(sed -n 's/^tproxy-port:[[:space:]]*\([0-9][0-9]*\).*/\1/p' "$cfg" | head -n 1)
    if [ -z "$value" ]; then
        value=$(tproxy_entry | awk '{print $1}')
    fi
    [ -n "$value" ] && printf ',"tproxy-port":%s' "$value"
    return 0
}

firewall_state() {
    [ -f "$FW" ] || { printf 'unknown'; return 0; }
    if sh "$FW" check --lan "$(setting lan-interface br0 quiet)" >/dev/null 2>&1; then
        printf 'up'
    else
        printf 'down'
    fi
}

# ---- pc 形态 ----

# pc 形态插件不装规则：本机透明代理只能来自 meow 的 tproxy-port 托管。
# 没配透明代理是合法用法（只当 mixed 代理用），不出声；只有“写了 tproxy 入口
# 却把规则推给谁都没装”这种真错配才告警。
pc_warnings() {
    _tp=$(cfg_value tproxy-port)
    case "$_tp" in
        ''|null)
            case " $(tproxy_entry) " in
                *" false "*)
                    echo "meow: pc 形态不装防火墙，但这个 tproxy 入口写了 firewall: false —— 本机透明代理不会生效。改用它顶层的 tproxy-port，或把 proxy-mode 改成 router" >&2 ;;
            esac ;;
    esac
}

# ---- router 形态 ----

load_router_params() {
    LAN_IF=$(setting lan-interface br0)
    set -- $(tproxy_entry)
    TPROXY_PORT=${1:-}
    TPROXY_FIREWALL=${2:-}
    TPROXY_LISTEN=${3:-}
    set -- $(dns_listen)
    DNS_ADDR=${1:-}
    DNS_PORT=${2:-}
    DNS_ENABLE=$(dns_enable)
    MARK=$(cfg_value routing-mark)
    case "$MARK" in null) MARK='' ;; esac
}

router_hint() {
    echo "meow: proxy-mode: router 需要一份网关配置，当前配置不满足：" >&2
    echo "  $*" >&2
    echo "  参考 README「网关形态（router）」里的配置片段。" >&2
}

validate_router() {
    if ! is_port "${TPROXY_PORT:-}"; then
        router_hint "listeners: 里缺 type: tproxy 的入口，或入口没写端口"
        return 1
    fi
    if [ "$TPROXY_FIREWALL" != false ]; then
        router_hint "tproxy 入口必须写 firewall: false —— 路由器内核没有 nf_tables，meow 内置防火墙起不来"
        return 1
    fi
    case "$TPROXY_LISTEN" in
        *:*) router_hint "tproxy 入口的 listen 是 IPv6（$TPROXY_LISTEN）；网关规则只做 v4 REDIRECT，写 0.0.0.0"; return 1 ;;
        127.*|::1|localhost)
            router_hint "tproxy 入口的 listen 是 '$TPROXY_LISTEN'，LAN 的 REDIRECT 到不了；写 0.0.0.0"
            return 1 ;;
        '')
            # 没写 listen 时 meow 用全局 bind-address（缺省 127.0.0.1）兜底，
            # 只有 allow-lan + 0.0.0.0 才会绑到 LAN；别靠这个默认值。
            if [ "$(cfg_value allow-lan)" != true ] || [ "$(cfg_value bind-address)" != 0.0.0.0 ]; then
                router_hint "tproxy 入口没写 listen，按当前 allow-lan/bind-address 会绑到 127.0.0.1；显式写 listen: 0.0.0.0"
                return 1
            fi ;;
    esac
    case "$(cfg_value tproxy-port)" in
        ''|null) ;;
        *) router_hint "不能同时写顶层 tproxy-port —— 那是 pc 形态的托管入口，会和 listeners 抢端口"; return 1 ;;
    esac

    case "$DNS_ENABLE" in
        true) ;;
        *) echo "meow: dns.enable 不是 true，跳过 DNS 劫持（LAN 客户端的 DNS 直连）" >&2; DNS_PORT='' ;;
    esac
    if is_port "${DNS_PORT:-}"; then
        case "$DNS_ADDR" in
            127.*|::1|localhost)
                router_hint "dns.listen 绑在 '$DNS_ADDR'，LAN 的 DNS 劫持会打到没人监听的地址；改成 0.0.0.0:$DNS_PORT"
                return 1 ;;
            *:*)
                router_hint "dns.listen 是 IPv6（$DNS_ADDR）；LAN 的 v4 DNS 劫持到不了，改成 0.0.0.0:$DNS_PORT"
                return 1 ;;
        esac
    elif [ -n "${DNS_ENABLE:-}" ] && [ "$DNS_ENABLE" = true ]; then
        echo "meow: 配置里没有可用的 dns.listen，跳过 DNS 劫持（只劫持 TCP）" >&2
        DNS_PORT=''
    fi
    if [ -z "$MARK" ]; then
        echo "meow: 配置里没有 routing-mark，跳过 SO_MARK 放行规则" >&2
    else
        case "$MARK" in
            *[!0-9]*) router_hint "routing-mark 不是数字：'$MARK'"; return 1 ;;
        esac
    fi
    case "$(cfg_value ipv6)" in
        true) echo "meow: ipv6: true 而规则只做 v4 REDIRECT，LAN 客户端的 AAAA 会绕过代理；网关形态建议 ipv6: false" >&2 ;;
    esac
    [ -e "/sys/class/net/$LAN_IF" ] || { router_hint "LAN 网口 '$LAN_IF' 不存在（改 lan-interface）"; return 1; }
    return 0
}

# 规则参数打包成 meow-fw.sh 的参数（函数内的 positional 参数不影响调用方）。
# up / down / status 都从这一处拼参数，顺序与 --dns / --mark 的取舍只写一次。
fw_call() {
    set -- "$1" --lan "$LAN_IF"
    if [ -n "${TPROXY_PORT:-}" ]; then set -- "$@" --tproxy "$TPROXY_PORT"; fi
    if [ -n "${DNS_PORT:-}" ]; then set -- "$@" --dns "$DNS_PORT"; fi
    if [ -n "${MARK:-}" ]; then set -- "$@" --mark "$MARK"; fi
    sh "$FW" "$@"
}

# 拆规则不看端口：meow-fw.sh 只按 --lan 找自建链，改过端口也能清干净。
fw_clear() {
    [ -f "$FW" ] || return 0
    sh "$FW" down --lan "$LAN_IF" >/dev/null 2>&1
    return 0
}

run_router() {
    [ -f "$FW" ] || { echo "meow: 缺少规则脚本 $FW" >&2; return 1; }
    # 厂商网络服务可能在别处把 ip_forward 刷回 0（meow-fw.sh up 里也会再设一次）。
    echo 1 > /proc/sys/net/ipv4/ip_forward 2>/dev/null \
        || echo "meow: 无法打开 ip_forward" >&2

    echo "meow starting mode=router (config=$CFG lan=$LAN_IF tproxy=:$TPROXY_PORT dns=:${DNS_PORT:-off})" >&2
    "$HERE/meow" -d "$ROOT" -f "$CFG" --ext-ui "$ROOT/yacd" &
    mon_pid=$!
    echo "$mon_pid" > "$PIDFILE"

    # 端口就绪之后再挂规则：规则先挂、进程没起，客户端会拿到 connection refused。
    n=0
    while [ "$n" -lt 60 ]; do
        port_listening "$TPROXY_PORT" && break
        if ! kill -0 "$mon_pid" 2>/dev/null; then
            echo "meow: 进程在监听前退出（配置或端口有问题）" >&2
            rm -f "$PIDFILE"
            return 1
        fi
        sleep 1
        n=$((n + 1))
    done
    if [ "$n" -ge 60 ]; then
        echo "meow: 60s 内 :$TPROXY_PORT 没有监听" >&2
        kill "$mon_pid" 2>/dev/null
        wait "$mon_pid" 2>/dev/null
        rm -f "$PIDFILE"
        return 1
    fi

    if ! fw_call up; then
        echo "meow: 装 nat 规则失败，停止 meow（不留半套规则）" >&2
        fw_clear
        kill "$mon_pid" 2>/dev/null
        wait "$mon_pid" 2>/dev/null
        rm -f "$PIDFILE"
        return 1
    fi

    # 阻塞在这里：svcd 靠「start 没返回」判断服务活着。
    wait "$mon_pid"
    rc=$?
    echo "meow exited rc=$rc" >&2
    # 进程没了就别留规则：宁可直连，也别让 LAN 一直打到死端口。
    fw_clear
    rm -f "$PIDFILE"
    return "$rc"
}

# ---- 生命周期 ----

do_start() {
    CFG=$(prepare_config) || return 1
    if running; then
        echo "already running (pid $(cat "$PIDFILE"), port 7890)" >&2
        return 1
    fi
    MODE=$(resolve_mode) || return 1

    # 先做一次配置校验；失败就不要 exec，宿主会看到 start 失败而不是一个秒退进程。
    if ! "$HERE/meow" -d "$ROOT" -f "$CFG" -t >/dev/null 2>&1; then
        echo 'meow: config validation failed (run with -t to see details)' >&2
        return 1
    fi

    if [ "$MODE" = pc ]; then
        pc_warnings
        if [ -f "$FW" ] && [ "$(firewall_state 2>/dev/null)" = up ]; then
            echo "meow: 还挂着网关的 nat 规则（上次 router 形态的残留），LAN 可能仍在被劫持；用 sh $0 fw down 清掉" >&2
        fi
        echo $$ > "$PIDFILE"
        echo "meow starting mode=pc (data=$ROOT, config=$CFG)" >&2
        exec "$HERE/meow" -d "$ROOT" -f "$CFG" --ext-ui "$ROOT/yacd"
    fi

    load_router_params
    validate_router || return 1
    run_router
}

do_stop() {
    CFG=$(prepare_config 2>/dev/null) || CFG="$CONFIG"
    # 先撤规则：撤完 LAN 立刻回到直连，不会打到一个没人监听的端口。
    # 不看 proxy-mode：从 router 切回 pc 之后也要能把旧规则清掉。
    if [ -f "$FW" ] && [ "$(firewall_state 2>/dev/null)" = up ]; then
        sh "$FW" down --lan "$(setting lan-interface br0 quiet)" >/dev/null 2>&1
    fi
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
    CFG=$(prepare_config 2>/dev/null) || CFG="$CONFIG"
    mode=$(resolve_mode quiet 2>/dev/null) || mode=pc
    fw=''
    if [ "$mode" = router ]; then
        state=$(firewall_state)
        fw=$(printf ',"firewall":"%s"' "$state")
        if running && [ "$state" != up ]; then
            echo "meow: running but nat rules are down (sh $0 fw up 或重跑 start)" >&2
        elif ! running && [ "$state" = up ]; then
            echo "meow: not running but nat rules are still up (sh $0 fw down)" >&2
        fi
    fi
    if running; then
        printf '{"running":true,"pid":%s,"mode":"%s","controller":"127.0.0.1:9090"%s%s}\n' \
            "$(cat "$PIDFILE")" "$mode" "$fw" "$(report_ports "$CFG")"
    else
        # 不要在这里删 pidfile：宿主可能在 start 写到一半时先跑 status，
        # 删掉会把刚起来的进程变成“看不见”。
        printf '{"running":false,"mode":"%s","controller":"127.0.0.1:9090"%s%s}\n' \
            "$mode" "$fw" "$(report_ports "$CFG")"
    fi
}

# 调试用：只按当前配置跑规则脚本，不动 meow 进程。
do_fw() {
    CFG=$(prepare_config 2>/dev/null) || CFG="$CONFIG"
    load_router_params
    _cmd=${1:-status}
    if [ "$_cmd" = up ]; then
        # 先校验：校验会按 dns.enable 修正 DNS_PORT，参数要在它之后才打包。
        validate_router || return 1
    fi
    fw_call "$_cmd"
}

# 调试用：打印解析出来的形态与参数，不启动任何东西。
do_mode() {
    CFG=$(prepare_config 2>/dev/null) || CFG="$CONFIG"
    echo "config: $CFG"
    _mode=$(resolve_mode quiet || echo '?')
    echo "mode  : $_mode"
    # 只有网关形态才需要这些参数；pc 形态去读 lan-interface 只会多一条
    # “配置里没有 …” 的兜底告警。
    [ "$_mode" = router ] || return 0
    load_router_params
    echo "lan   : ${LAN_IF:-}"
    echo "tproxy: ${TPROXY_PORT:-} (firewall=${TPROXY_FIREWALL:-?} listen=${TPROXY_LISTEN:-?})"
    echo "dns   : ${DNS_ADDR:-} ${DNS_PORT:-}"
    echo "mark  : ${MARK:-}"
}

case "${1:-}" in
start)  do_start ;;
stop)   do_stop ;;
status) do_status ;;
mode)   do_mode ;;
fw)     shift; do_fw "$@" ;;
*)      echo "usage: $0 start|stop|status|mode|fw [up|down|status]" >&2; exit 2 ;;
esac
