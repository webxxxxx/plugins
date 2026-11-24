#!/bin/sh
# meow：包装 vendor/meow-rs 编译出来的二进制。
#
# 两个文件，各管各的：
#   <插件目录>/config     我们的设置（manifest 的 config 指向它）。meow 不读、也不改，
#                         所以 proxy-mode / mixed-port 这类开关永远不会被它重写掉。
#   <插件目录>/meow.yaml  meow 自己的配置（订阅、规则、代理组）。我们只在启动时改它
#                         受管的几个键（mixed-port / tun / dns / tproxy-port），其余
#                         原样保留；第一次启动从包里的 payload/config.yaml 生成。
#
# 运行形态由 config 里的 proxy-mode 决定（缺省 pc），受管键按形态自动写进 meow.yaml：
#   off    不接管流量：只开 mixed-port 当普通 HTTP/SOCKS 代理，tproxy / tun 都不开。
#   pc     本机透明代理。Linux：tproxy-port + routing-mark（meow 自己装 nft 规则）；
#          Windows：tun + fake-ip（见 meow.ps1）。页面上的「tun」就是它。
#   router 网关：iptables 规则由 meow-fw.sh 成对管理，meow 只在 0.0.0.0:<tproxy-port>
#          上开 tproxy 入口（需要 root + iptables；没有 nf_tables 的内核也能用）。
#          页面上的「tproxy」就是它。
#
# 宿主把 lifecycle.start 当成常驻前台进程：start 一直不返回，直到 meow 退出；
# stop/status 执行后退出。路径全部从脚本位置推导，数据目录用 SVCD_DATA_DIR。
# 清单里 exec.ready 是 notify：宿主先发一条 svcd.ping，收到回复才算启动成功，之后
# 这条路就是能力通道（ready: exec 的 stdout 只进日志，页面调不到任何能力）。
#
# start 现在是「看护进程」而不是把 meow exec 掉：meow 是自己的子进程，通道能力跑在
# 后台（页面 A 的代理类型选择器调它），孩子退出后前台循环按 config 里的新形态重启。
# 这样页面改 proxy-mode 不需要额外进程，也不必重启插件。
set -u

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "$HERE/.." && pwd)          # payload/：yacd、cn_domains 在这一层
PLUGIN_ROOT=$(CDPATH= cd -- "$HERE/../../.." && pwd)
BASE=${SVCD_DATA_DIR:-$PLUGIN_ROOT/.data}
DATA="$BASE/meow"
PIDFILE="$DATA/meow.pid"
CONFIG="$PLUGIN_ROOT/config"                     # 我们的设置
MEOW_CONFIG="$PLUGIN_ROOT/meow.yaml"             # meow 的配置
TEMPLATE="$ROOT/config.yaml"                     # 包里的 meow 配置模板
FW="$HERE/meow-fw.sh"
SDK="$ROOT/lib/svcd-plugin.sh"                   # 宿主 SDK：通道循环与能力分发

mkdir -p "$DATA" 2>/dev/null || true

# 能力通道（页面 A 的代理类型选择器）用 SDK；sync 会把 sdk/shell 的副本刷到 payload/lib。
if [ -f "$SDK" ]; then . "$SDK"; fi

# ---- 我们的设置 ----

# 顶层标量：只认行首的 `键: 值`，去掉行尾注释、引号和首尾空白。
our() {
    sed -n "s/^$1:[[:space:]]*\([^#]*\).*/\1/p" "$CONFIG" 2>/dev/null \
        | head -n 1 \
        | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' -e 's/^"//' -e 's/"$//' -e "s/^'//" -e "s/'$//"
}

# 我们的设置里取一个键，缺省用第二个参数。
our_or() {
    _value=$(our "$1")
    if [ -z "$_value" ] || [ "$_value" = null ]; then
        printf '%s' "$2"
    else
        printf '%s' "$_value"
    fi
}

mode_of() {
    _mode=$(our proxy-mode)
    case "$_mode" in
        '') printf 'pc' ;;
        off|pc|router) printf '%s' "$_mode" ;;
        *) echo "meow: config 里 proxy-mode 只能是 off、pc 或 router（现在是 '$_mode'）" >&2; return 1 ;;
    esac
}

# 页面上的三个选项（代理类型）与实际形态的对应：tun ↔ pc，tproxy ↔ router。
label_of() {
    case "${1:-}" in
        pc) printf 'tun' ;;
        router) printf 'tproxy' ;;
        *) printf 'off' ;;
    esac
}

mode_of_label() {
    case "${1:-}" in
        off) printf 'off' ;;
        tun) printf 'pc' ;;
        tproxy) printf 'router' ;;
        *) return 1 ;;
    esac
}

# 网关形态（iptables + tproxy 入口）只有 Linux 有。
is_linux() {
    case "$(uname -s 2>/dev/null)" in
        Linux) return 0 ;;
    esac
    return 1
}

is_port() {
    case "${1:-}" in
        ''|*[!0-9]*) return 1 ;;
    esac
    [ "$1" -ge 1 ] && [ "$1" -le 65535 ]
}

# ---- meow.yaml 的小编辑 ----
# 只做三件事：改顶层标量、删顶层键、改块内的标量。注释、顺序和缩进都原样保留，
# 因为这份文件里还有订阅和规则，不能被重新序列化。

# set_key <文件> <键> <值>：有就替换那一行，没有就追加到末尾。
set_key() {
    _tmp="$1.tmp.$$"
    awk -v key="$2" -v val="$3" '
        BEGIN { done = 0 }
        $0 ~ "^" key ":[ \t]*" { print key ": " val; done = 1; next }
        { print }
        END { if (!done) print key ": " val }
    ' "$1" > "$_tmp" && mv "$_tmp" "$1"
}

# del_key <文件> <键>：删掉键本身和它下面的缩进行（块也跟着走）。
del_key() {
    _tmp="$1.tmp.$$"
    awk -v key="$2" '
        BEGIN { skip = 0 }
        /^[^ \t#]/ {
            skip = ($0 ~ ("^" key ":[ \t]*"))
            if (skip) next
            print; next
        }
        { if (!skip) print }
    ' "$1" > "$_tmp" && mv "$_tmp" "$1"
}

# set_block_key <文件> <块> <键> <值>：块里那一行替换，块里没有就补一行；
# 连块都没有就把整块追加到末尾。
set_block_key() {
    _tmp="$1.tmp.$$"
    awk -v block="$2" -v key="$3" -v val="$4" '
        function flush() {
            if (inblock && !done) { print "  " key ": " val; done = 1 }
        }
        BEGIN { inblock = 0; seen = 0; done = 0 }
        /^[^ \t#]/ {
            flush()
            inblock = ($0 ~ ("^" block ":[ \t]*$"))
            if (inblock) seen = 1
            print; next
        }
        {
            if (inblock) {
                if ($0 ~ ("^[ \t]+" key ":[ \t]*")) { print "  " key ": " val; done = 1; next }
            }
            print
        }
        END {
            flush()
            if (!seen) {
                print ""
                print block ":"
                print "  " key ": " val
            }
        }
    ' "$1" > "$_tmp" && mv "$_tmp" "$1"
}

# 这份文件看起来是不是 meow 的配置（旧布局把两者放在同一个文件里）。
looks_like_meow_config() {
    [ -f "$1" ] && grep -qE '^(proxies|proxy-groups|rules|mixed-port|tun|listeners):' "$1"
}

write_our_config() {
    {
        echo "# meow 插件的设置：meow 不读这个文件，也不会改它。"
        echo "# 改完 plugin restart meow 生效。"
        echo "proxy-mode: ${1:-pc}"
        echo "mixed-port: 7890"
        echo ""
        echo "# 透明代理端口（Linux pc / router 形态用；Windows 走 TUN，不看这个）。"
        echo "tproxy-port: 7893"
        echo "# meow 自己出向的 SO_MARK，避免被自己的规则再抓一次（Linux pc）。"
        echo "routing-mark: 9527"
        echo ""
        echo "# router 形态：被劫持的 LAN 网口，以及劫持 DNS 用的端口。"
        echo "lan-interface: br0"
        echo "dns-port: 1053"
    } > "$CONFIG"
}

# 保证 meow.yaml 存在：旧布局（订阅写在 config 里）就迁过来，否则用包里的模板。
prepare_config() {
    if [ -f "$MEOW_CONFIG" ]; then
        return 0
    fi
    if looks_like_meow_config "$CONFIG"; then
        _mode=$(our proxy-mode)
        cp "$CONFIG" "$MEOW_CONFIG" || return 1
        # 迁过来之后 config 恢复成“我们的设置”，原来写的 proxy-mode 留着。
        write_our_config "${_mode:-pc}"
        echo "meow: 已把旧配置迁到 $MEOW_CONFIG，设置留在 $CONFIG" >&2
        return 0
    fi
    [ -f "$TEMPLATE" ] || { echo 'meow: payload/config.yaml missing' >&2; return 1; }
    sed -e "s#@DATA@#$DATA#g" "$TEMPLATE" > "$MEOW_CONFIG" || return 1
    return 0
}

# router 形态要一个 tproxy 入口：有就沿用（端口以它为准），没有就补一条。
# 补的时候插在 listeners: 下面；连 listeners 块都没有就整块追加。
ensure_tproxy_listener() {
    _port=$1
    [ -n "$(tproxy_entry)" ] && return 0
    if grep -qE '^listeners:[ \t]*$' "$MEOW_CONFIG"; then
        _tmp="$MEOW_CONFIG.tmp.$$"
        awk -v port="$_port" '
            { print }
            /^listeners:[ \t]*$/ && !done {
                print "  - name: tproxy"
                print "    type: tproxy"
                print "    port: " port
                print "    listen: 0.0.0.0"
                print "    firewall: false"
                done = 1
            }
        ' "$MEOW_CONFIG" > "$_tmp" && mv "$_tmp" "$MEOW_CONFIG"
        return 0
    fi
    {
        echo ""
        echo "listeners:"
        echo "  - name: tproxy"
        echo "    type: tproxy"
        echo "    port: $_port"
        echo "    listen: 0.0.0.0"
        echo "    firewall: false"
    } >> "$MEOW_CONFIG"
    return 0
}

# 按形态把受管键写进 meow.yaml。这是“启动即用”的实现：用户只用写 proxy-mode 和
# 端口，tun / fake-ip / tproxy-port / routing-mark 都由这里补齐。
apply_settings() {
    _mode=$1
    _port=$(our_or mixed-port 7890)
    is_port "$_port" || { echo "meow: mixed-port 不是端口：'$_port'" >&2; return 1; }
    set_key "$MEOW_CONFIG" mixed-port "$_port"
    # 订阅里带进来的 tun 块要关掉：三种形态的透明代理都不靠订阅自带的 tun。
    if grep -qE '^tun:' "$MEOW_CONFIG"; then
        set_block_key "$MEOW_CONFIG" tun enable false
    fi

    case "$_mode" in
    off)
        # 不接管：只留 mixed-port。tproxy-port 一去掉，meow 就不装 nft 规则。
        del_key "$MEOW_CONFIG" tproxy-port
        del_key "$MEOW_CONFIG" routing-mark
        ;;
    pc)
        _tproxy=$(our_or tproxy-port 7893)
        is_port "$_tproxy" || { echo "meow: tproxy-port 不是端口：'$_tproxy'" >&2; return 1; }
        set_key "$MEOW_CONFIG" tproxy-port "$_tproxy"
        _mark=$(our_or routing-mark 9527)
        case "$_mark" in
            *[!0-9]*) echo "meow: routing-mark 只能是数字：'$_mark'" >&2; return 1 ;;
        esac
        set_key "$MEOW_CONFIG" routing-mark "$_mark"
        ;;
    router)
        # 入口是 listeners 里的 tproxy 项，顶层 tproxy-port 会和它抢端口。
        del_key "$MEOW_CONFIG" tproxy-port
        del_key "$MEOW_CONFIG" routing-mark
        set_block_key "$MEOW_CONFIG" dns enable true
        _dns=$(our_or dns-port 1053)
        is_port "$_dns" || { echo "meow: dns-port 不是端口：'$_dns'" >&2; return 1; }
        set_block_key "$MEOW_CONFIG" dns listen "0.0.0.0:$_dns"
        _tproxy=$(our_or tproxy-port 7893)
        is_port "$_tproxy" || { echo "meow: tproxy-port 不是端口：'$_tproxy'" >&2; return 1; }
        ensure_tproxy_listener "$_tproxy"
        ;;
    *)
        echo "meow: 未知形态 '$_mode'" >&2
        return 1
        ;;
    esac
    return 0
}

# ---- router 形态 ----

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
    ' "$MEOW_CONFIG"
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
    }' "$MEOW_CONFIG"
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
    }' "$MEOW_CONFIG"
}

load_router_params() {
    LAN_IF=$(our_or lan-interface br0)
    set -- $(tproxy_entry)
    TPROXY_PORT=${1:-}
    TPROXY_FIREWALL=${2:-}
    TPROXY_LISTEN=${3:-}
    set -- $(dns_listen)
    DNS_ADDR=${1:-}
    DNS_PORT=${2:-}
    DNS_ENABLE=$(dns_enable)
    MARK=$(our routing-mark)
    case "$MARK" in null) MARK='' ;; esac
}

router_hint() {
    echo "meow: proxy-mode: router 需要一份网关配置，当前配置不满足：" >&2
    echo "  $*" >&2
    echo "  参考 README「网关形态（router）」里的配置片段。" >&2
}

validate_router() {
    if ! is_port "${TPROXY_PORT:-}"; then
        router_hint "meow.yaml 的 listeners: 里没有 type: tproxy 的入口（内置入口时要写 port）"
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
    else
        echo "meow: 没有可用的 dns.listen，跳过 DNS 劫持（只劫持 TCP）" >&2
        DNS_PORT=''
    fi
    if [ -z "$MARK" ]; then
        echo "meow: config 里没有 routing-mark，跳过 SO_MARK 放行规则" >&2
    else
        case "$MARK" in
            *[!0-9]*) router_hint "routing-mark 不是数字：'$MARK'"; return 1 ;;
        esac
    fi
    case "$(sed -n 's/^ipv6:[[:space:]]*\([a-z]*\).*/\1/p' "$MEOW_CONFIG" | head -n 1)" in
        true) echo "meow: ipv6: true 而规则只做 v4 REDIRECT，LAN 客户端的 AAAA 会绕过代理；网关形态建议 ipv6: false" >&2 ;;
    esac
    [ -e "/sys/class/net/$LAN_IF" ] || { router_hint "LAN 网口 '$LAN_IF' 不存在（改 config 里的 lan-interface）"; return 1; }
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
    # 不看 proxy-mode、也不看当前端口：只按 --lan 找自建链，切形态后也能清干净。
    sh "$FW" down --lan "$(our_or lan-interface br0)" >/dev/null 2>&1
    return 0
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
    for key in mixed-port port socks-port; do
        value=$(sed -n "s/^${key}:[[:space:]]*\([0-9][0-9]*\).*/\1/p" "$MEOW_CONFIG" | head -n 1)
        [ -n "$value" ] && printf ',"%s":%s' "$key" "$value"
    done
    value=$(sed -n 's/^tproxy-port:[[:space:]]*\([0-9][0-9]*\).*/\1/p' "$MEOW_CONFIG" | head -n 1)
    if [ -z "$value" ]; then
        value=$(tproxy_entry | awk '{print $1}')
    fi
    [ -n "$value" ] && printf ',"tproxy-port":%s' "$value"
    return 0
}

firewall_state() {
    [ -f "$FW" ] || { printf 'unknown'; return 0; }
    if sh "$FW" check --lan "$(our_or lan-interface br0)" >/dev/null 2>&1; then
        printf 'up'
    else
        printf 'down'
    fi
}

# 起一个 meow 子进程，pidfile 记下它。router 形态还要等 tproxy 端口起来再挂规则
# （规则先挂、进程没起，客户端会拿到 connection refused）。
start_child() {
    _mode=$1
    if [ "$_mode" = router ] && [ ! -f "$FW" ]; then
        echo "meow: 缺少规则脚本 $FW" >&2
        return 1
    fi
    echo "meow starting mode=$_mode (config=$MEOW_CONFIG)" >&2
    # stdin 接 /dev/null：宿主的通道口只留在看护进程里，meow 不该继承它。
    "$HERE/meow" -d "$ROOT" -f "$MEOW_CONFIG" --ext-ui "$ROOT/yacd" >&2 < /dev/null &
    mon_pid=$!
    echo "$mon_pid" > "$PIDFILE"
    [ "$_mode" = router ] || return 0
    # 厂商网络服务可能在别处把 ip_forward 刷回 0（meow-fw.sh up 里也会再设一次）。
    echo 1 > /proc/sys/net/ipv4/ip_forward 2>/dev/null \
        || echo "meow: 无法打开 ip_forward" >&2

    # 端口就绪之后再挂规则。
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

    echo "meow router ready: lan=$LAN_IF tproxy=:$TPROXY_PORT dns=:${DNS_PORT:-off}" >&2
    return 0
}

# 收掉子进程和它留下的规则。幂等：宿主跑 lifecycle stop 时这里可能已经收过一遍。
cleanup_child() {
    [ -n "${mon_pid:-}" ] || { rm -f "$PIDFILE"; return 0; }
    kill "$mon_pid" 2>/dev/null
    rm -f "$PIDFILE"
    # 进程没了就别留规则：宁可直连，也别让 LAN 一直打到死端口。
    fw_clear
    mon_pid=''
    return 0
}

# 前台看护：孩子退出后，如果 config 里的 proxy-mode 变了就是页面上的主动切换，按新形态
# 重来一次；没变就是真退出，把 rc 交给宿主（宿主据此重启或报错）。
supervise() {
    while :; do
        MODE=$(mode_of) || return 1
        apply_settings "$MODE" || return 1
        if [ "$MODE" = router ]; then
            load_router_params
            validate_router || return 1
        elif ! "$HERE/meow" -d "$ROOT" -f "$MEOW_CONFIG" -t >/dev/null 2>&1 < /dev/null; then
            # pc/off 形态先做一次配置校验，失败就别起一个秒退进程。
            echo 'meow: config validation failed (run meow -t to see details)' >&2
            return 1
        fi
        start_child "$MODE" || { cleanup_child; return 1; }
        wait "$mon_pid"
        _rc=$?
        echo "meow exited rc=$_rc (mode=$MODE)" >&2
        cleanup_child
        _next=$(mode_of 2>/dev/null) || _next=''
        if [ -n "$_next" ] && [ "$_next" != "$MODE" ]; then
            echo "meow: proxy-mode $MODE → $_next，按新形态重启" >&2
            continue
        fi
        return "$_rc"
    done
}

# ---- 能力：代理类型（页面 A 的顶部选择器） ----
# 不带参数是读，带 mode 是写。写只改 config 的 proxy-mode 并结束孩子进程，
# 重启由前台看护循环负责——所以一次调用里不会出现「配置已改、进程还没换」的状态。

# 读：{"mode":"off|tun|tproxy","proxy-mode":"off|pc|router","running":bool,...}
report_mode() {
    _mode=$(mode_of 2>/dev/null) || _mode=pc
    _running=false
    running && _running=true
    _ports=''
    [ -f "$MEOW_CONFIG" ] && _ports=$(report_ports)
    if is_linux; then
        _allowed='"off","tun","tproxy"'
    else
        _allowed='"off","tun"'
    fi
    printf '{"mode":"%s","proxy-mode":"%s","running":%s,"allowed":[%s]%s}\n' \
        "$(label_of "$_mode")" "$_mode" "$_running" "$_allowed" "$_ports"
}

# 写：把页面上的 off/tun/tproxy 落成 proxy-mode，持久化在 config 里。
do_set_mode() {
    _want=$1
    _target=$(mode_of_label "$_want") || {
        echo "meow: 代理类型只能是 off / tun / tproxy（收到 '$_want'）" >&2
        return 1
    }
    if [ "$_target" = router ] && ! is_linux; then
        echo "meow: tproxy（网关形态）只能在 Linux 上开" >&2
        return 1
    fi
    _now=$(mode_of 2>/dev/null) || _now=''
    if [ "$_now" = "$_target" ] && running; then
        report_mode
        return 0
    fi
    prepare_config || return 1
    set_key "$CONFIG" proxy-mode "$_target" || return 1
    # 孩子活着就叫它退出：前台看护循环醒来会看到新的 proxy-mode，按新形态重启。
    _pid=$(cat "$PIDFILE" 2>/dev/null || true)
    if [ -n "$_pid" ]; then
        kill "$_pid" 2>/dev/null || true
    fi
    printf '{"mode":"%s","changing":true}\n' "$_want"
}

handle() {
    case "$1" in
    mode) handle_mode "$2" ;;
    *) echo "unknown tool: $1" >&2; return 1 ;;
    esac
}

handle_mode() {
    _want=$(svcd_field "${1:-}" mode)
    if [ -z "$_want" ]; then
        report_mode
        return 0
    fi
    do_set_mode "$_want"
}

# 宿主 `svc status` 的一行摘要。
svcd_status() {
    _mode=$(mode_of 2>/dev/null) || _mode=pc
    if running; then
        printf '%s (mode=%s)' 'running' "$(label_of "$_mode")"
    else
        printf '%s (mode=%s)' 'meow is not running' "$(label_of "$_mode")"
    fi
}

cleanup_on_exit() {
    trap '' TERM INT HUP EXIT
    [ -n "${CHANNEL:-}" ] && kill "$CHANNEL" 2>/dev/null
    cleanup_child
}

# ---- 生命周期 ----

do_start() {
    prepare_config || return 1
    if running; then
        echo "already running (pid $(cat "$PIDFILE"))" >&2
        return 1
    fi
    rm -f "$PIDFILE"
    if [ ! -f "$SDK" ]; then
        echo "meow: 缺少通道 SDK（$SDK），页面上的代理类型开关不可用" >&2
        return 1
    fi

    SELF=$$
    trap 'cleanup_on_exit' EXIT
    trap 'exit 0' TERM INT HUP
    # 后台作业的 stdin 会被 shell 接到 /dev/null，所以先把宿主的通道口存成 fd 5 再给它。
    exec 5<&0
    # 通道能力跑在后台：它只改写 config 并结束孩子，重启交给下面的前台循环。
    # SDK 收到 svcd.shutdown 会自己 exit，所以「通道断了就叫醒看护循环」只能挂在 EXIT 上。
    ( trap 'kill -TERM "$SELF" 2>/dev/null' EXIT; svcd_plugin_serve handle <&5 ) &
    CHANNEL=$!
    supervise
    rc=$?
    exit "$rc"
}

do_stop() {
    CFG_MODE=$(mode_of 2>/dev/null) || CFG_MODE=pc
    # 先撤规则：撤完 LAN 立刻回到直连，不会打到一个没人监听的端口。
    # 不看 proxy-mode：从 router 切回 pc 之后也要能把旧规则清掉。
    if [ -f "$FW" ] && [ "$(firewall_state 2>/dev/null)" = up ]; then
        LAN_IF=$(our_or lan-interface br0)
        sh "$FW" down --lan "$LAN_IF" >/dev/null 2>&1
    fi
    if running; then
        pid=$(cat "$PIDFILE")
        kill "$pid" 2>/dev/null || true
        n=0
        while kill -0 "$pid" 2>/dev/null && [ "$n" -lt 5 ]; do
            sleep 1
            n=$((n + 1))
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
    mode=$(mode_of 2>/dev/null) || mode=pc
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
    ports=''
    if [ -f "$MEOW_CONFIG" ]; then
        ports=$(report_ports)
    fi
    proxy=$(printf ',"proxy":"%s"' "$(label_of "$mode")")
    if running; then
        printf '{"running":true,"pid":%s,"mode":"%s"%s,"controller":"127.0.0.1:9090"%s%s}\n' \
            "$(cat "$PIDFILE")" "$mode" "$proxy" "$fw" "$ports"
    else
        # 不要在这里删 pidfile：宿主可能在 start 写到一半时先跑 status，
        # 删掉会把刚起来的进程变成“看不见”。
        printf '{"running":false,"mode":"%s"%s,"controller":"127.0.0.1:9090"%s%s}\n' \
            "$mode" "$proxy" "$fw" "$ports"
    fi
}

# 调试用：只按当前配置跑规则脚本，不动 meow 进程。
do_fw() {
    prepare_config || return 1
    _mode=$(mode_of) || return 1
    apply_settings "$_mode" || return 1
    load_router_params
    _cmd=${1:-status}
    if [ "$_cmd" = up ]; then
        validate_router || return 1
    fi
    fw_call "$_cmd"
}

# 调试用：打印解析出来的形态与参数，不启动任何东西。
# 会先把受管键写一遍（和 start 一样），所以看到的就是 meow 实际要用的配置。
do_mode() {
    prepare_config || return 1
    echo "config: $CONFIG"
    echo "meow  : $MEOW_CONFIG"
    _mode=$(mode_of) || return 1
    echo "mode  : $_mode"
    apply_settings "$_mode" || return 1
    # 只有网关形态才需要这些参数；pc 形态去读 lan-interface 只会多一条告警。
    [ "$_mode" = router ] || return 0
    load_router_params
    echo "lan   : ${LAN_IF:-}"
    echo "tproxy: ${TPROXY_PORT:-} (firewall=${TPROXY_FIREWALL:-?} listen=${TPROXY_LISTEN:-?})"
    echo "dns   : ${DNS_ADDR:-} ${DNS_PORT:-}"
    echo "mark  : ${MARK:-}"
}

case "${1:-}" in
    start)    do_start ;;
    stop)     do_stop ;;
    status)   do_status ;;
    mode)     do_mode ;;
    set-mode) shift; do_set_mode "${1:-}" ;;
    fw)       shift; do_fw "$@" ;;
    *)        echo "usage: $0 start|stop|status|mode|set-mode off|tun|tproxy|fw [up|down|status]" >&2; exit 2 ;;
esac
