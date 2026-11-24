#!/bin/sh
# meow 的配置层：
#   <插件目录>/config  meow 自己的配置（订阅/规则/端口），meow 就是用它启动的
#   <插件目录>/state   我们自己的，只缓存代理类型（off / pc / router）
#
# 用一个假布局（伪 meow / 伪规则脚本）跑 meow.sh 的配置相关子命令，检查三件事：
# 受管键按形态写进了 meow 的配置、代理类型落在 state、迁移把老布局收拾干净。
# 用法：sh tools/meow-config-test.sh [仓库根]
set -eu

REPO=$(CDPATH= cd -- "${1:-$(dirname -- "$0")/..}" && pwd)
WORK=$(mktemp -d "${TMPDIR:-/tmp}/meow-config-test.XXXXXX")
trap 'rm -rf "$WORK"' EXIT

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

meow_config() {
    cat <<'YAML'
# 订阅配置（示例）
mixed-port: 7891
proxies:
  - name: node-1
    type: direct
tun:
  enable: true
  auto-route: true
dns:
  enable: true
  listen: 127.0.0.1:5353
  nameserver:
    - 223.5.5.5
rules:
  - MATCH,DIRECT
YAML
}

# 假插件目录：<root>/plugin/{config,state} + <root>/plugin/1.3.0/payload/{bin,lib,config.yaml}
fixture() {
    _root=$1
    rm -rf "$_root"
    _plugin="$_root/plugin"
    _bin="$_plugin/1.3.0/payload/bin"
    mkdir -p "$_bin" "$_plugin/1.3.0/payload/yacd" "$_plugin/1.3.0/payload/lib"
    cp "$REPO/plugins/meow/platforms/meow.sh" "$_bin/meow.sh"
    cp "$REPO/plugins/meow/platforms/lib/svcd-plugin.sh" "$_plugin/1.3.0/payload/lib/svcd-plugin.sh"
    cp "$REPO/plugins/meow/package/payload/config.yaml" "$_plugin/1.3.0/payload/config.yaml"
    meow_config > "$_plugin/config"
    # 伪 meow：校验（-t）立刻成功，真正启动就赖着不退，好让看护循环有东西可等。
    printf '#!/bin/sh\ncase " $* " in *" -t "*) exit 0 ;; esac\nwhile :; do sleep 1; done\n' > "$_bin/meow"
    printf '#!/bin/sh\necho "fw-stub: $*"\n' > "$_bin/meow-fw.sh"
    chmod +x "$_bin/meow" "$_bin/meow.sh" "$_bin/meow-fw.sh"
}

run_meow() {
    SVCD_DATA_DIR="$_plugin/.data" sh "$_plugin/1.3.0/payload/bin/meow.sh" "$@" 2>&1
}

state_of() {
    sed -n 's/^proxy-mode:[[:space:]]*//p' "$_plugin/state" 2>/dev/null | head -n 1
}

# ---- pc：tproxy 端口和 mark 写进去，tun 关掉，代理类型落进 state ----
fixture "$WORK/pc"
cp "$_plugin/config" "$WORK/config.before"
run_meow mode >/dev/null
grep -qx 'mixed-port: 7891' "$_plugin/config" || fail 'pc: 用户写的 mixed-port 被改掉了'
grep -qx 'tproxy-port: 7893' "$_plugin/config" || fail 'pc: tproxy-port 没写进去'
grep -qx 'routing-mark: 9527' "$_plugin/config" || fail 'pc: routing-mark 没写进去'
grep -A1 -x 'tun:' "$_plugin/config" | grep -qx '  enable: false' || fail 'pc: tun 没关掉'
grep -q 'node-1' "$_plugin/config" || fail 'pc: 订阅内容被弄丢了'
grep -qx 'proxy-mode: pc' "$_plugin/state" || fail 'pc: state 里没有 proxy-mode'
cmp -s "$WORK/config.before" "$_plugin/state" && fail 'pc: state 跟配置混在一起了'

# 用户改 mixed-port，跟着走（配置是 meow 的，端口以它为准）
sed -i 's/^mixed-port: 7891/mixed-port: 1080/' "$_plugin/config"
run_meow mode >/dev/null
grep -qx 'mixed-port: 1080' "$_plugin/config" || fail 'pc: 改 config 的 mixed-port 没生效'

# ---- off：只留 mixed-port，tproxy/mark/tun 全撤 ----
fixture "$WORK/off"
run_meow set-mode off >/dev/null
grep -qx 'proxy-mode: off' "$_plugin/state" || fail 'off: state 里的代理类型没改成 off'
run_meow mode >/dev/null
grep -qx 'mixed-port: 7891' "$_plugin/config" || fail 'off: mixed-port 被动过'
grep -qx 'tproxy-port:.*' "$_plugin/config" && fail 'off: 不该有 tproxy-port'
grep -qx 'routing-mark:.*' "$_plugin/config" && fail 'off: 不该有 routing-mark'
grep -A1 -x 'tun:' "$_plugin/config" | grep -qx '  enable: false' || fail 'off: tun 没关掉'

# ---- 页面上的三个值 ↔ 形态：tun=pc / tproxy=router / off=off ----
fixture "$WORK/labels"
run_meow set-mode tun >/dev/null
[ "$(state_of)" = pc ] || fail 'labels: tun 没落成 pc'
if [ "$(uname -s)" = Linux ]; then
    run_meow set-mode tproxy >/dev/null
    [ "$(state_of)" = router ] || fail 'labels: tproxy 没落成 router'
else
    # 网关形态只有 Linux 有；别的平台要明确拒绝，而不是写进 state。
    if run_meow set-mode tproxy >/dev/null 2>&1; then fail 'labels: 非 Linux 上 tproxy 应该被拒'; fi
    [ "$(state_of)" = pc ] || fail 'labels: 被拒的 tproxy 改了 state'
fi
run_meow set-mode off >/dev/null
[ "$(state_of)" = off ] || fail 'labels: off 没落成 off'
if run_meow set-mode turbo >/dev/null 2>&1; then fail 'labels: 非法值应该被拒'; fi
[ "$(state_of)" = off ] || fail 'labels: 非法值把 state 改了'
run_meow status | grep -q '"proxy":"off"' || fail 'labels: status 没报出代理类型'

# ---- router：删掉顶层 tproxy-port，DNS 绑 0.0.0.0，自带一条 tproxy 入口 ----
fixture "$WORK/router"
# 直接写缓存：网关形态的平台检查不走这里，这里只测受管键怎么写。
printf 'proxy-mode: router\n' > "$_plugin/state"
run_meow fw status >/dev/null
grep -qx 'tproxy-port:.*' "$_plugin/config" && fail 'router: 顶层 tproxy-port 应该被删掉'
grep -qx '  listen: 0.0.0.0:1053' "$_plugin/config" || fail 'router: dns.listen 没改成 0.0.0.0:1053'
grep -qx '    type: tproxy' "$_plugin/config" || fail 'router: 没补 tproxy 入口'
grep -qx '    firewall: false' "$_plugin/config" || fail 'router: tproxy 入口没写 firewall: false'
grep -qx '  enable: false' "$_plugin/config" || fail 'router: tun 没关掉'

# ---- 重复跑要幂等 ----
cp "$_plugin/config" "$WORK/once.yaml"
run_meow fw status >/dev/null
cmp -s "$WORK/once.yaml" "$_plugin/config" || fail 'router: 第二次跑改了文件'

# ---- 迁移：1.2/1.3 的 meow.yaml 搬回 config，旧开关搬进 state ----
fixture "$WORK/migrate"
meow_config > "$_plugin/meow.yaml"
cat > "$_plugin/config" <<'OLD'
# meow 插件的设置：meow 不读这个文件，也不会改它。
proxy-mode: off
mixed-port: 7890
tproxy-port: 7893
routing-mark: 9527
lan-interface: br-lan
dns-port: 1053
OLD
run_meow mode >/dev/null
[ -f "$_plugin/meow.yaml" ] && fail 'migrate: meow.yaml 没搬走'
grep -q 'node-1' "$_plugin/config" || fail 'migrate: 订阅没搬进 config'
grep -qx 'proxy-mode: off' "$_plugin/state" || fail 'migrate: 旧开关没搬进 state'
grep -qx 'lan-interface: br-lan' "$_plugin/state" || fail 'migrate: lan-interface 没搬进 state'
grep -qx 'proxy-mode:.*' "$_plugin/config" && fail 'migrate: config 里还留着我们的键'

# ---- 迁移：更老的布局（开关和订阅写在一起）——把我们的键摘出去 ----
fixture "$WORK/migrate-old"
meow_config | sed -e 's/^mixed-port: 7891/mixed-port: 7891\nproxy-mode: pc\nlan-interface: br-wan/' > "$_plugin/config"
run_meow mode >/dev/null
grep -qx 'proxy-mode:.*' "$_plugin/config" && fail 'migrate-old: proxy-mode 还留在 config 里'
grep -qx 'lan-interface:.*' "$_plugin/config" && fail 'migrate-old: lan-interface 还留在 config 里'
grep -qx 'lan-interface: br-wan' "$_plugin/state" || fail 'migrate-old: lan-interface 没搬进 state'

# ---- 通道：页面调 mode 读状态、切代理类型，看护循环按新形态重启 ----
# 串起 meow.sh start 的完整形态：帧从 stdin 进，回复从 stdout 出，切完必须自己重启。
# 只在 Linux 上跑：MSYS/Cygwin 的 fork + 信号模型撑不起「后台读管道 + 前台 wait」，测出来的
# 是仿真层的脾气，不是脚本的行为。CI 与 WSL 都走 Linux 这一支。
if [ "$(uname -s)" != Linux ]; then
    echo "skip: 通道测试只在 Linux 上跑（当前 $(uname -s)）" >&2
else
fixture "$WORK/channel"
cat > "$WORK/feed.sh" <<'FEED'
sleep 1
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"mode","arguments":{}}}'
sleep 1
printf '%s\n' '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"mode","arguments":{"mode":"off"}}}'
sleep 3
printf '%s\n' '{"jsonrpc":"2.0","id":3,"method":"svcd.shutdown"}'
FEED
MEOW_SH="$_plugin/1.3.0/payload/bin/meow.sh"
if timeout 25 sh -c 'sh "$1" | SVCD_DATA_DIR="$2" sh "$3" start' sh \
    "$WORK/feed.sh" "$_plugin/.data" "$MEOW_SH" \
    > "$WORK/channel.out" 2> "$WORK/channel.err"; then
    :
else
    fail "channel: 看护进程没干净退出（$(cat "$WORK/channel.err")）"
fi
channel_has() {
    grep -qF "$1" "$WORK/channel.out" && return 0
    {
        echo "--- channel.out"
        cat "$WORK/channel.out"
        echo "--- channel.err"
        cat "$WORK/channel.err"
    } >&2
    fail "channel: $2"
}

# 能力结果是 JSON 里的字符串，引号会被转义：文件里是 \"mode\":\"tun\"。
channel_has 'mode\":\"tun\"' '读状态没回代理类型'
channel_has 'changing\":true' '切代理类型没回执'
[ "$(state_of)" = off ] || fail 'channel: 切完没落到 state'
grep -q 'proxy-mode pc → off' "$WORK/channel.err" || fail 'channel: 看护循环没按新形态重启'
[ -f "$_plugin/.data/meow/meow.pid" ] && fail 'channel: 退出后还留着 pidfile'
fi

echo "meow config layer: ok"
