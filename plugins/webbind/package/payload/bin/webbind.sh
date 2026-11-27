#!/bin/sh
# webbind: 把厂商后台左侧的「应用插件」菜单注入原厂后台。
#
# 标准 svcd 插件：宿主监督的进程就是本脚本（exec.args = start）。
#   start  注入一次（生成 app.*.js 的覆盖副本并 bind 到 /home/httpd/public），
#          然后常驻：每 60 秒确认覆盖还挂着，被卸掉的就补回来
#   stop   进程消失之后收尾：卸掉覆盖挂载（厂商后台还原）
#   status 一行 JSON，给 `svc status` 的 probe 用
#
# **只依赖插件自己的文件**：
#   - 注入正文是包里的 payload/menu.js（不是设备上的任何副本）；
#   - 生成的覆盖副本放在插件自己的 data/ 里（$PLUGIN_ROOT/data/httpd/…）；
#   - 设备路径写死（见下面的常量），没有配置文件，也没有可调项。
#
# 为什么常驻：宿主按“被监督的进程还在不在”判断服务；一次性跑完就退出的进程会让插件
# 立刻变成 stopped。常驻循环只比对挂载表（不进文件、不写盘），挂载被卸掉时自动补回来。
#
# 厂商后台的「应用插件」菜单打开的是 svcd 控制台 http://<host>:8787/。厂商后台已经
# 登录时不该再登一次：注入的 menu.js 在浏览器里用厂商会话调 svcd 的 POST /vendor-login
# 换一个 svcd 会话（跨端口同主机，cookie 同域）。那扇门开不开归宿主管：svcd.conf 的
# `vendor_login = on`（默认 off，见 plugin-manager ADR-0032），本插件不碰宿主的配置。
#
# 本插件在插件目录之外**只做 bind mount**（挂载是内核状态，不是文件）：不创建、不修改
# 设备上的任何文件；唯一会写的目录是自己安装目录下的 data/。
#
# 只随 aarch64-unknown-linux-musl / compat.devices = sr6110 的包发布：厂商后台的
# app.*.js 布局和 loginStatus 的 _tag 都是这台设备写死的。
#
# 日志一律走 stderr（宿主收进 <日志目录>/webbind.log）；不自造日志/运行态文件。
set -u

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
# 宿主给插件的安装目录；手跑时按脚本位置往上三层推（<插件>/<版本>/payload/bin/…）
PLUGIN_ROOT=$(CDPATH= cd -- "${SVCD_PLUGIN_DIR:-$HERE/../../..}" && pwd)

# ---- 写死的设备路径（没有配置；这台设备只有这一套布局） -------------------
# 厂商 httpd 的 web 根。只读 jffs2，所以覆盖得靠 bind mount。
HTTPD=/home/httpd/public
# 注入正文：只认包内这份。
MENU=$HERE/../menu.js
# 生成的覆盖副本放插件自己的 data/：不依赖设备的覆盖目录，升级/卸载（不带 --purge）
# 都保留它，重新 start 会覆盖。
DATA=$PLUGIN_ROOT/data/httpd
# 挂载表。
MOUNTS=/proc/mounts
# 看护间隔（秒）：只比挂载表，成本很低。
WATCH_SECS=60

# 菜单注入的标记：先删上一次注入的块再写，重复执行不会叠加。
MENU_BEGIN=srfix-menu-inject
MENU_END='end srfix-menu-inject'

export PATH=/etc/sr/bin:/bin:/sbin:/usr/bin:/usr/sbin

log() { echo "webbind: $*" >&2; }

# 解析真实路径（busybox 的 readlink 可能没有 -f）
_realpath() {
    [ -e "$1" ] || { echo ""; return 1; }
    ( cd "$(dirname "$1")" 2>/dev/null && printf '%s/%s\n' "$(pwd -P)" "$(basename "$1")" )
}

# <目标> 上有没有挂载（挂载表第二列是目标，与 bind 的源无关）
_mounted() {
    [ -r "$MOUNTS" ] || return 1
    while read -r _dev _t _rest; do
        [ "$_t" = "$1" ] && return 0
    done < "$MOUNTS"
    return 1
}

# 这个挂载目标是不是本插件挂的（只会是原厂 static/js 下的覆盖文件）。
# 为什么按目标而不按源：/proc/mounts 里 bind 挂载的源字段是底层设备
# （/dev/root 之类），不是 `mount --bind` 的源路径，按源比永远对不上。
_is_our_target() {
    _pub=$(_realpath "$HTTPD" 2>/dev/null)
    [ -n "$_pub" ] && case "$1" in "$_pub"/jquery/static/js/*) return 0 ;; esac
    case "$1" in
        "$HTTPD"/jquery/static/js/*) return 0 ;;
    esac
    return 1
}

# 卸载本插件挂上去的覆盖。先收集再卸：卸挂载会改挂载表，边读边卸会漏。
web_unbind() {
    [ -r "$MOUNTS" ] || return 0
    _targets=$(
        while read -r _dev _t _rest; do
            _is_our_target "$_t" && printf '%s\n' "$_t"
        done < "$MOUNTS"
    )
    _n=0
    for _t in $_targets; do
        if umount "$_t" 2>/dev/null; then
            log "unbound $_t"
            _n=$((_n + 1))
        elif umount -l "$_t" 2>/dev/null; then
            # 浏览器还开着被覆盖的文件时普通 umount 会 EBUSY；lazy 先摘掉，
            # 引用一放完内核就收，比留着覆盖挂载好。
            log "lazy-unbound $_t"
            _n=$((_n + 1))
        else
            log "unmount failed: $_t"
        fi
    done
    [ "$_n" -gt 0 ] && log "unbound $_n mount(s)"
    return 0
}

# 用包里的 menu.js 生成原厂 app.*.js 的覆盖副本（先删上一次注入的块，幂等）。
# 返回生成了几份（全局 _MENU_N）。
web_menu() {
    _MENU_N=0
    [ -f "$MENU" ] || { log "no menu payload at $MENU"; return 1; }
    for _v in "$HTTPD"/jquery/static/js/app.*.js; do
        [ -f "$_v" ] || continue
        _rel=${_v#"$HTTPD"/}
        _out=$DATA/$_rel
        if ! mkdir -p "$(dirname "$_out")" 2>/dev/null; then
            log "menu: cannot create $(dirname "$_out")"
            continue
        fi
        # 以原厂内容做底；文件里已经有我们注入的块时先删掉，避免重复注入
        if ! sed "/$MENU_BEGIN/,/$MENU_END/d" "$_v" > "$_out" 2>/dev/null; then
            log "menu: cannot read $_v"
            continue
        fi
        printf '\n/* --- %s: plugin menu --- */\n' "$MENU_BEGIN" >> "$_out"
        cat "$MENU" >> "$_out"
        printf '/* --- %s --- */\n' "$MENU_END" >> "$_out"
        log "menu: injection prepared for $_rel"
        _MENU_N=$((_MENU_N + 1))
    done
    [ "$_MENU_N" -gt 0 ] || log "menu: no vendor app.*.js under $HTTPD/jquery/static/js"
    return 0
}

# 把 data/ 里的覆盖副本逐个 bind 到原厂同名文件。绑定失败数在 _BIND_BAD。
# 已经挂着就跳过：看护循环每轮都会调它，不能叠层。
bind_overrides() {
    _BIND_BAD=0
    [ -d "$DATA" ] || return 0
    for _f in $(find "$DATA" -type f 2>/dev/null); do
        _rel=${_f#"$DATA"/}
        _dst=$(_realpath "$HTTPD/$_rel")
        if [ -z "$_dst" ] || [ ! -f "$_dst" ]; then
            log "override skipped (no vendor file): $_rel"
            continue
        fi
        if _mounted "$_dst"; then
            continue
        fi
        if mount --bind "$_f" "$_dst" 2>/dev/null; then
            log "override bound: $_rel"
        else
            log "override bind failed: $_rel"
            _BIND_BAD=$((_BIND_BAD + 1))
        fi
    done
    return 0
}

# 完整跑一遍：先卸残留，再生成覆盖副本、逐个挂上。
apply() {
    web_unbind
    web_menu || return 1
    bind_overrides
    if [ "$_BIND_BAD" -gt 0 ]; then
        log "apply: $_BIND_BAD override(s) did not bind"
        return 1
    fi
    return 0
}

# 看护：只补挂载，不重写覆盖副本（副本是 apply 时生成的）。
ensure() {
    bind_overrides >/dev/null 2>&1
}

do_start() {
    apply || log "start: 有失败项，看护循环会继续尝试补挂"
    log "watching every ${WATCH_SECS}s (pid $$)"
    while :; do
        sleep "$WATCH_SECS"
        ensure
    done
}

do_stop() {
    # 宿主负责终止进程（进程组 SIGTERM → 10 秒 → SIGKILL）；到这里进程已经不在了，
    # 只做收尾：卸载覆盖。卸载幂等，强杀留下的残留也能收。
    web_unbind
    log "stopped"
}

do_status() {
    _total=0
    _on=0
    if [ -d "$DATA" ]; then
        for _f in $(find "$DATA" -type f 2>/dev/null); do
            _rel=${_f#"$DATA"/}
            _total=$((_total + 1))
            _dst=$(_realpath "$HTTPD/$_rel" 2>/dev/null)
            if [ -n "$_dst" ] && _mounted "$_dst"; then
                _on=$((_on + 1))
            fi
        done
    fi
    _menu=false
    [ -f "$MENU" ] && _menu=true
    printf '{"overrides":%s,"overrides_bound":%s,"menu":%s,"pid":%s}\n' \
        "$_total" "$_on" "$_menu" "$$"
}

[ "${WEBBIND_NO_MAIN:-0}" = "1" ] || {
case "${1:-start}" in
    start)  do_start ;;
    stop)   do_stop ;;
    status) do_status ;;
    apply)  apply ;;
    help|-h|--help)
        cat <<'EOF'
webbind - 把「应用插件」菜单注入厂商后台（svcd 标准插件）

用法:
  webbind.sh start    注入一次，然后常驻看护（宿主 exec.args = start）
  webbind.sh stop     卸载覆盖挂载（进程消失后收尾）
  webbind.sh status   一行 JSON（overrides / menu / pid）
  webbind.sh apply    立即重做一遍（调试用）

固定路径（没有配置文件）:
  /home/httpd/public                    厂商 httpd 的 web 根
  /home/httpd/public/jquery/static/js/app.*.js   原厂后台脚本
  <插件>/payload/menu.js                注入正文（只认包内这份）
  <插件>/data/httpd/<rel>               生成的覆盖副本

在插件目录之外只做 bind mount，不创建/修改任何文件。
控制台免登录归宿主：svcd.conf 的 `vendor_login = on`（ADR-0032）。

只随 aarch64-unknown-linux-musl / compat.devices = sr6110 发布。
EOF
        ;;
    *) echo "usage: $0 start|stop|status|apply" >&2; exit 2 ;;
esac
}
