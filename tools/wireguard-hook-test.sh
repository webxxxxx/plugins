#!/bin/sh
# wireguard 的能力钩子（svcd #342）：清单里 exec.ready 是 exec，管理页面的五个能力是
# 短命钩子——宿主每次 tools/call 起一次 payload/bin/wireguardd.sh hook <能力>，arguments
# 从 stdin 读，我们的 stdout 就是这次调用的文本结果。这里验三件事：
#   1. state 读得出来（settings / peers / status 三段都在）；
#   2. 不认识的能力非零退出，而且原因写在 stdout（fail 在钩子里也走 stdout）；
#   3. status.guardian 按 pidfile 认自己的进程（/proc/<pid>/cmdline），pid 被复用给
#      别的进程时报 false。
#
# 只在 Linux 上跑：认进程靠 /proc，跟 tools/lifecycle-test.sh 同一套。
# 用法：sh tools/wireguard-hook-test.sh [仓库根]
set -eu

REPO=$(CDPATH= cd -- "${1:-$(dirname -- "$0")/..}" && pwd)
WORK=$(mktemp -d "${TMPDIR:-/tmp}/wireguard-hook-test.XXXXXX")
DECOY=''
WATCH=''

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

cleanup() {
    [ -n "$DECOY" ] && kill "$DECOY" 2>/dev/null
    [ -n "$WATCH" ] && kill "$WATCH" 2>/dev/null
    rm -rf "$WORK"
    return 0
}
trap cleanup EXIT INT TERM

# 假插件目录：<root>/plugin/payload/{bin,lib} + <root>/plugin/data
fixture() {
    _root=$1
    rm -rf "$_root"
    _plugin="$_root/plugin"
    _bin="$_plugin/payload/bin"
    mkdir -p "$_bin" "$_plugin/payload/lib" "$_plugin/data"
    cp "$REPO/plugins/wireguard/package/payload/bin/wireguardd.sh" "$_bin/"
    cp "$REPO/plugins/wireguard/package/payload/bin/wg-lan-fix.sh" "$_bin/"
    cp "$REPO/plugins/wireguard/package/payload/lib/wireguard-lib.sh" "$_plugin/payload/lib/"
    cp "$REPO/plugins/wireguard/package/payload/lib/svcd-plugin.sh" "$_plugin/payload/lib/"
    printf '#!/bin/sh\nexit 0\n' > "$_bin/wg-tool"
    chmod 755 "$_bin/wg-tool"
}

hook() {    # $1 = 能力名，$2 = arguments JSON
    printf '%s\n' "$2" | SVCD_PLUGIN_DIR="$_plugin" sh "$_bin/wireguardd.sh" hook "$1"
}

# ---- state：设置、peer 列表、运行状态一起给页面 ----
fixture "$WORK/state"
if ! out=$(hook state '{}' 2> "$WORK/state.err"); then
    fail "state: 退出非零（$(cat "$WORK/state.err")）"
fi
case "$out" in
*'"settings":{'*'"peers":['*'"status":{'*) ;;
*) fail "state: 返回里缺段（$out）" ;;
esac
case "$out" in
*'"guardian":false'*) ;;
*) fail "state: 没有看守在跑，guardian 该是 false（$out）" ;;
esac

# ---- 不认识的能力：非零退出，原因在 stdout（fail 在钩子里也写 stdout）----
if hook nope '{}' > "$WORK/nope.out" 2> "$WORK/nope.err"; then
    fail 'nope: 不认识的能力没报错'
fi
grep -q 'unknown tool' "$WORK/nope.out" ||
    fail "nope: 失败原因没写到 stdout（$(cat "$WORK/nope.out")）"

# ---- save_settings：十个参数从 stdin 读出来、落盘、回执 ----
fixture "$WORK/save"
cat > "$_plugin/data/settings" <<'SET'
WG_LAN_IF=br0
WG_PORT=51820
WG_NET=10.99.0.0/24
WG_ADDR=10.99.0.1
WG_MTU=1420
WG_TABLE=200
WG_ENDPOINT=vpn.example.com
WG_DNS=223.5.5.5
WG_ALLOWED_IPS=0.0.0.0/0,::/0
WG_KEEPALIVE=25
WG_IMPL=auto
WG_LAN_FIX=auto
SET
# 只改连接地址（不动接口/端口/网段/地址/MTU）：落盘就行，不用重启插件。
if ! out=$(hook save_settings '{"lan_interface":"br0","port":51820,"network":"10.99.0.0/24","address":"10.99.0.1","mtu":1420,"table":200,"endpoint":"vpn2.example.com","dns":"223.5.5.5","allowed_ips":"0.0.0.0/0,::/0","keepalive":25}' 2> "$WORK/save.err"); then
    fail "save_settings: 退出非零（$(cat "$WORK/save.err")）"
fi
case "$out" in
*'"ok":true'*) ;;
*) fail "save_settings: 没回 ok（$out）" ;;
esac
case "$out" in
*'"restart":false'*) ;;
*) fail "save_settings: 不该要重启（$out）" ;;
esac
grep -qx 'WG_ENDPOINT=vpn2.example.com' "$_plugin/data/settings" ||
    fail "save_settings: 新设置没落盘（$(grep WG_ENDPOINT "$_plugin/data/settings")）"

# 改监听端口：隧道要重建。钩子只把设置落盘并回 restart=true —— 它自己不起进程、
# 也不杀进程组，重建由页面调宿主的 restart 做。
if ! out=$(hook save_settings '{"lan_interface":"br0","port":51821,"network":"10.99.0.0/24","address":"10.99.0.1","mtu":1420,"table":200,"endpoint":"vpn2.example.com","dns":"223.5.5.5","allowed_ips":"0.0.0.0/0,::/0","keepalive":25}' 2> "$WORK/save-port.err"); then
    fail "save_settings: 改端口时退出非零（$(cat "$WORK/save-port.err")）"
fi
case "$out" in
*'"restart":true'*) ;;
*) fail "save_settings: 改端口没要重启（$out）" ;;
esac
grep -qx 'WG_PORT=51821' "$_plugin/data/settings" ||
    fail 'save_settings: 新端口没落盘'

# ---- wg-lan-fix.sh：看守用 --once（一拍就走，不另养一个常驻进程）----
fixture "$WORK/lan-fix"
SVCD_PLUGIN_DIR="$_plugin" sh "$_bin/wg-lan-fix.sh" --once > "$WORK/once.out" 2> "$WORK/once.err" || true
if grep -q '自愈循环启动' "$WORK/once.err"; then
    fail 'lan-fix: --once 不该进循环'
fi
# 手跑（调试）还是循环：日志能证明它走的是循环那个分支。
SVCD_PLUGIN_DIR="$_plugin" sh "$_bin/wg-lan-fix.sh" > /dev/null 2> "$WORK/loop.err" || true
grep -q '自愈循环启动' "$WORK/loop.err" || fail 'lan-fix: 手跑模式没进循环'

# ---- guardian：pidfile 里那个 pid 必须是自己的进程 ----
# 活的、但命令行不是 wireguardd.sh 的进程（pid 被复用给别的进程）：不能认。
sleep 60 &
DECOY=$!
printf '%s\n' "$DECOY" > "$_plugin/data/wireguardd.pid"
case "$(hook state '{}')" in
*'"guardian":false'*) ;;
*) fail 'guardian: 无关进程被当成了看守' ;;
esac

# 命令行里带 wireguardd.sh 的进程（$0 冒充）：认成看守。
# `sleep` 后面跟一条命令，免得 sh 把 sleep exec 掉、把 $0 丢掉。
sh -c 'sleep 60; :' wireguardd.sh &
WATCH=$!
printf '%s\n' "$WATCH" > "$_plugin/data/wireguardd.pid"
case "$(hook state '{}')" in
*'"guardian":true'*) ;;
*) fail "guardian: 看守在跑却报 false（$(hook state '{}')）" ;;
esac
kill "$WATCH" 2>/dev/null || true
WATCH=''

echo 'wireguard capability hooks: ok'
