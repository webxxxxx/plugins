#!/bin/bash
# ==========================================================
# webbind 插件的本地测试（不需要设备 / root）
#
#   ./plugins/webbind/tests/webbind.sh
#
# 覆盖：
#   1. 契约：清单/平台/分支、无配置文件、路径写死、只依赖包内文件
#   2. 菜单注入：覆盖副本内容、幂等（重复执行 / 原厂文件已带旧块）、没有 menu.js
#   3. apply / stop：真挂载用假 mount/umount + 假挂载表；幂等、按目标卸载、别人的不动
#   4. 失败与安全跳过：bind 失败、没有原厂 app.js
#   5. 插件目录之外不写文件（只在自己的 data/ 里生成覆盖副本）
#   6. 看护：绑定被卸掉之后 ensure 把它补回来（不重写覆盖副本）
#
# 不碰真的 /proc/mounts：source 之后把 MOUNTS 指到临时文件、mount/umount 换成假命令。
# 假挂载表的源字段照真机写底层设备（不是 mount --bind 的源路径），
# 免得测试跑在“按源认领”这种假前提下。
# ==========================================================
set -u

HERE=$(cd "$(dirname "$0")" && pwd)
PLUGIN=$(cd "$HERE/.." && pwd)
SCRIPT="$PLUGIN/package/payload/bin/webbind.sh"
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

PASS=0
FAIL=0
ok()  { echo "  ✓ $*"; PASS=$((PASS+1)); }
bad() { echo "  ✗ $*"; FAIL=$((FAIL+1)); }
eq()  { if [ "$1" = "$2" ]; then ok "$3（$1）"; else bad "$3（期望 $2，实际 $1）"; fi; }

[ -f "$SCRIPT" ] || { echo "missing $SCRIPT"; exit 1; }

# ---- 假世界 ---------------------------------------------------------------
W="$TMP/httpd/public"
mkdir -p "$W/jquery/static/js" "$TMP/svcd"
printf 'console.log("spa");\n' > "$W/jquery/static/js/app.96c81c9.js"
: > "$TMP/mounts"

FB="$TMP/fakebin"
mkdir -p "$FB"
cat > "$FB/mount" <<'EOF'
#!/bin/sh
if [ "${FAKE_MOUNT_FAIL:-0}" = "1" ]; then exit 1; fi
[ "$1" = "--bind" ] || { echo "fake mount: only --bind" >&2; exit 64; }
printf '%s %s none rw,bind 0 0\n' "/dev/fakeroot" "$3" >> "$WEBBIND_MOUNTS"
EOF
cat > "$FB/umount" <<'EOF'
#!/bin/sh
awk -v t="$1" '$2 != t' "$WEBBIND_MOUNTS" > "$WEBBIND_MOUNTS.new" &&
    mv "$WEBBIND_MOUNTS.new" "$WEBBIND_MOUNTS"
EOF
chmod +x "$FB/mount" "$FB/umount"

# 假 mount/umount 通过这个环境变量找到挂载表（脚本里的 MOUNTS 变量）
export WEBBIND_MOUNTS="$TMP/mounts"
export WEBBIND_NO_MAIN=1
ORIG_PATH=$PATH
# shellcheck disable=SC1090
. "$SCRIPT"
# 脚本自己会导出设备的 PATH；source 之后再接回假命令和本机工具
export PATH="$FB:$ORIG_PATH"
# 设备路径是写死的常量；测试只临时改这几个全局来模拟设备布局。
# MENU 也要显式指一次：脚本是用 $0 推自己的位置的，被 source 时 $0 是测试脚本。
HTTPD="$W"
DATA="$TMP/data"
MENU="$PLUGIN/package/payload/menu.js"
MOUNTS="$TMP/mounts"

app="$W/jquery/static/js/app.96c81c9.js"
ovr="$DATA/jquery/static/js/app.96c81c9.js"

echo
echo "=========================================="
echo " 1. 插件声明：无配置、路径写死、自包含"
echo "=========================================="
MAN="$PLUGIN/package/manifest.json"
grep -q '"id": "webbind"' "$MAN" && ok "清单 id = webbind" || bad "清单 id 不对"
grep -q '"ready": "exec"' "$MAN" && ok "exec.ready=exec（无协议通道，常驻看护）" || bad "ready 不对"
grep -q '"sr6110"' "$MAN" && ok "compat.devices 只声明 sr6110" || bad "缺 compat.devices sr6110"
grep -q 'aarch64-unknown-linux-musl' "$PLUGIN/.plugin.json" && ok "只有 aarch64-unknown-linux-musl" || bad "平台不是只有 aarch64"
[ ! -e "$PLUGIN/package/config" ] && ok "包里没有 config（不提供配置）" || bad "包里还有 config 目录"
grep -q '"config"' "$MAN" && bad "清单还声明了 config" || ok "清单不声明 config"
grep -q '^HTTPD=/home/httpd/public$' "$SCRIPT" && ok "厂商 web 根写死为 /home/httpd/public" || bad "HTTPD 没写死"
grep -q 'WEB_SRC\|WEB_OV\|WEB_PUBLIC\|WEB_PAGE' "$SCRIPT" && bad "脚本里还有可配置的 WEB_* 变量" || ok "没有可配置的 WEB_* 变量"
grep -qF 'MENU=$HERE/../menu.js' "$SCRIPT" && ok "注入正文只认包内 payload/menu.js" || bad "MENU 没指向包内"
grep -q '/etc/sr/web' "$SCRIPT" && bad "还依赖 /etc/sr/web（固件 seed 的文件）" || ok "不依赖 /etc/sr/web"
eq "$(grep -cE '^[[:space:]]*(start|stop|status)\)' "$SCRIPT")" "3" "start/stop/status 各有一个独占行首的分支"
grep -qE '/var/(log|run)/' "$SCRIPT" && bad "脚本里还有自造的日志/运行态路径" || ok "不自造日志/运行态路径（日志走 stderr）"
[ -x "$SCRIPT" ] && ok "脚本 755（包内要可执行）" || bad "脚本没有可执行位"
PROBE="$PLUGIN/package/payload/menu.js"
[ -f "$PROBE" ] && ok "包内有 menu.js" || bad "包里缺 payload/menu.js"
grep -q ':8787' "$PROBE" 2>/dev/null && ok "menu.js 指向 :8787" || bad "menu.js 地址不对"

echo
echo "=========================================="
echo " 2. 菜单注入：覆盖副本 + 幂等"
echo "=========================================="
web_menu
[ -f "$ovr" ] && ok "在插件 data/ 里生成了覆盖副本" || bad "没有覆盖副本"
eq "$(grep -c 'srfix-menu-inject' "$ovr" 2>/dev/null)" "2" "注入块有起止两个标记"
grep -q '应用插件' "$ovr" && ok "内联了包内 menu.js 正文" || bad "覆盖副本里没有 menu.js"
cmp -s "$app" <(printf 'console.log("spa");\n') && ok "原厂文件没被改" || bad "原厂 app.js 被改了"
cp "$ovr" "$TMP/first.js"
web_menu
cmp -s "$ovr" "$TMP/first.js" && ok "重复执行内容不变（幂等）" || bad "重复执行后内容变了"

printf '\n/* --- srfix-menu-inject: plugin menu --- */\nOLD_RUN();\n/* --- end srfix-menu-inject --- */\n' >> "$app"
web_menu
eq "$(grep -c 'srfix-menu-inject' "$ovr")" "2" "原厂带旧块时重新生成仍只有两个标记"
grep -q 'OLD_RUN' "$ovr" && bad "旧块没删掉" || ok "旧块被按标记剥离"
printf 'console.log("spa");\n' > "$app"

(
    MENU="$TMP/no-menu.js" DATA="$TMP/data-none" web_menu
) >/dev/null 2>&1
[ "$?" -ne 0 ] && ok "没有 menu.js -> 明确失败" || bad "没有 menu.js 却报成功"
[ ! -e "$TMP/data-none/jquery/static/js/app.96c81c9.js" ] && ok "没有 menu.js 时什么都不生成" || bad "还是生成了覆盖"

echo
echo "=========================================="
echo " 3. apply / stop：挂载、幂等、按目标卸载"
echo "=========================================="
: > "$TMP/mounts"
apply
eq "$?" "0" "apply 退出 0"
eq "$(wc -l < "$TMP/mounts" | tr -d ' ')" "1" "只挂了覆盖文件（不再挂老插件页目录）"
grep -q '^/dev/fakeroot ' "$TMP/mounts" && ok "假挂载表照真机写设备名作源" || bad "假挂载表不符合真机格式"
grep -q "app.96c81c9.js" "$TMP/mounts" && ok "覆盖文件挂上了" || bad "覆盖文件没挂"
apply
eq "$(wc -l < "$TMP/mounts" | tr -d ' ')" "1" "再次 apply 不重复挂载"

printf 'tmpfs /tmp tmpfs rw 0 0\n' >> "$TMP/mounts"
do_stop >/dev/null 2>&1
grep -q "app.96c81c9.js" "$TMP/mounts" && bad "stop 没卸覆盖文件" || ok "stop 卸了覆盖文件（源是设备名也认得）"
grep -q '^tmpfs /tmp ' "$TMP/mounts" && ok "别人的挂载没被动" || bad "把别人的挂载卸了"
[ -f "$ovr" ] && ok "覆盖副本本身保留（下次 start 复用）" || bad "覆盖副本被删了"

# 目标上已经有挂载（上一轮残留/别人挂的）：不叠着挂，apply 也不因此失败
: > "$TMP/mounts"
printf '/dev/other %s none rw 0 0\n' "$(_realpath "$HTTPD/jquery/static/js/app.96c81c9.js")" >> "$TMP/mounts"
apply >/dev/null 2>&1
eq "$?" "0" "目标已有挂载时 apply 成功（跳过而不是报错）"
eq "$(grep -c 'app.96c81c9.js' "$TMP/mounts")" "1" "没有往已有挂载上再叠一层"

echo
echo "=========================================="
echo " 4. 失败与安全跳过"
echo "=========================================="
: > "$TMP/mounts"
(
    FAKE_MOUNT_FAIL=1 apply
) >/dev/null 2>&1
[ "$?" -ne 0 ] && ok "bind 失败 -> apply 非 0" || bad "bind 失败时 apply 仍是 0"
: > "$TMP/mounts"
mkdir -p "$TMP/empty-httpd"
(
    HTTPD="$TMP/empty-httpd" DATA="$TMP/data-empty" apply
) >/dev/null 2>&1
eq "$?" "0" "没有原厂 app.js -> 安全跳过"
[ ! -e "$TMP/data-empty" ] && ok "没挂上时不留任何副本目录" || bad "没挂上也建了副本目录"

echo
echo "=========================================="
echo " 5. 插件目录之外不写文件"
echo "=========================================="
# 只允许写自己 data/ 里的覆盖副本；不碰宿主的配置/日志/运行态，也不写 vendor-auth
grep -q 'vendor-auth\|/etc/svcd\|/etc/sr/web' "$SCRIPT" \
    && bad "脚本里还有插件目录之外的写路径" || ok "没有插件目录之外的写路径"
grep -qF 'DATA=$PLUGIN_ROOT/data/' "$SCRIPT" && ok "DATA 在插件自己的目录里" || bad "DATA 不在插件目录里"
# 真跑一遍：厂商 web 树里的文件只能被 bind 覆盖，字节不许变；别处也不许冒新文件
SNAP=$(mktemp -d)
trap 'rm -rf "$TMP" "$SNAP"' EXIT
snapshot() { find "$W" -type f -exec md5sum {} + 2>/dev/null | sort; }
snapshot > "$SNAP/before.txt"
: > "$TMP/mounts"
apply >/dev/null 2>&1
do_stop >/dev/null 2>&1
snapshot > "$SNAP/after.txt"
if diff -q "$SNAP/before.txt" "$SNAP/after.txt" >/dev/null 2>&1; then
    ok "厂商树里的文件没被改写（只 bind）"
else
    bad "厂商树里的文件被改了：$(diff "$SNAP/before.txt" "$SNAP/after.txt" | head -4 | tr '\n' ' ')"
fi
[ -d "$DATA" ] && [ -f "$ovr" ] && ok "唯一写的地方是插件自己的 data/" || bad "data/ 里没有覆盖副本"

echo
echo "=========================================="
echo " 6. 看护：绑定被卸掉后 ensure 补回来"
echo "=========================================="
: > "$TMP/mounts"
apply >/dev/null 2>&1
: > "$TMP/mounts"          # 模拟被外部卸掉（挂载表里没了，文件还在）
ensure >/dev/null 2>&1
eq "$(wc -l < "$TMP/mounts" | tr -d ' ')" "1" "ensure 把覆盖挂载补回来了"
cp "$ovr" "$TMP/before-ensure.js"
ensure >/dev/null 2>&1
cmp -s "$ovr" "$TMP/before-ensure.js" && ok "ensure 不重写覆盖副本" || bad "ensure 重写了覆盖副本"

echo
echo "=========================================="
echo " 通过: $PASS   失败: $FAIL"
echo "=========================================="
[ "$FAIL" = "0" ] || exit 1
