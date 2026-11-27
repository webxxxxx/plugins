# webbind — 厂商后台插件入口

把厂商后台左侧的「应用插件」菜单注入原厂后台。`/home/httpd/public` 是只读 jffs2，
写不进去：本插件用自带的 `menu.js` 生成 `app.*.js` 的覆盖副本，再 `mount --bind`
到原厂同名文件上。

标准 svcd 插件：宿主监督的就是 `exec`（`payload/bin/webbind.sh start`），常驻进程
负责注入 + 看护；`lifecycle.stop` 在进程消失后卸载还原。**不是** oneshot、也**不是**
core 脚本。

**只依赖插件自己的文件**：注入正文是包里的 `payload/menu.js`；生成的覆盖副本放在
插件自己的 `data/httpd/`；设备路径写死，没有配置文件、没有可调项。

只支持 `aarch64-unknown-linux-musl`，清单声明 `compat.devices: ["sr6110"]`
（产物名带 `-sr6110` 后缀）。菜单的 DOM 注入与登录交接都写死了这台设备的厂商后台
（`jquery/static/js/app.*.js`、`loginStatus` 的 `_tag`），不要当通用插件发。

## 布局

```
build.json                {"build": null}：没有编译步骤
package/
├── manifest.json         生命周期、compat.devices
└── payload/
    ├── bin/webbind.sh    start / stop / status / apply
    └── menu.js           注入正文（新宿主的唯一来源）
tests/webbind.sh          本地测试（假 mount/umount + 假挂载表，不需要设备）
```

## 固定路径（写死，没有配置）

| 路径 | 用途 |
|---|---|
| `/home/httpd/public` | 厂商 httpd 的 web 根（只读，所以只能 bind 覆盖） |
| `/home/httpd/public/jquery/static/js/app.*.js` | 原厂后台脚本 |
| `<插件>/payload/menu.js` | 注入正文 |
| `<插件>/data/httpd/<rel>` | 生成的覆盖副本（插件 data 目录，升级保留） |

插件目录**之外只做 `mount --bind`**（挂载是内核状态，不是文件）：不创建、不修改设备上的
任何文件，也不碰宿主的配置/运行态目录。

## 生命周期

| 动作 | 做什么 |
|---|---|
| `exec`（`start`） | 卸掉上次残留的覆盖挂载 → 用包内 `menu.js` 生成覆盖副本 → 逐个 `mount --bind` 到原厂 `app.*.js`。然后常驻：每 60 秒只比对挂载表，被卸掉的覆盖补回来（不重写文件）。 |
| `lifecycle.stop` | 进程消失**之后**收尾：卸掉本插件的覆盖挂载（按目标路径认领：`public/jquery/static/js/` 下；`/proc/mounts` 里 bind 的源是底层设备，按源认领永远对不上）。厂商后台立刻回到原厂样子。 |
| `lifecycle.status` | 一行 JSON：`overrides` / `overrides_bound` / `menu` / `pid`，给 `svc status` 的 probe 用。 |
| `apply`（手动） | 立刻重做一遍（调试用，不常驻）。 |

`exec.ready = "exec"`：没有协议通道，spawn 成功即 ready；`capabilities` 为空。
覆盖副本是覆盖挂载的源：`start`/`apply` 重新生成过它，所以原厂后台随固件升级换了
`app.<hash>.js` 名字时，重启插件就会按新名字重新注入（旧覆盖文件留在 data 里，不再挂载）。

## 日志与数据

- 日志全部走 stderr，宿主收进 `<日志目录>/webbind.log`；脚本不自造 `/var/log`、
  不写 pid 文件。
- 唯一的持久数据是 `data/httpd/` 里的覆盖副本；删掉它也不影响，`start` 会重建。

## 菜单打开的是 svcd 控制台（`:8787`）与免登录

注入的 `menu.js` 把面板 iframe 指向 `http://<host>:8787/`（新宿主 svcd 的控制台）。
厂商后台已经登录时，`:8787` 不该再问一次密码：

1. `menu.js`（厂商 `:80` 同源页面）带着浏览器 cookie 问厂商自己的 `loginStatus`
   （`/?_type=vueData&_tag=home_internetreg_lua_no_update_sess`）；
2. 只有答 `SUCC` 才 `POST http://<host>:8787/vendor-login`（`X-Vendor-OK: 1`、
   `credentials: include`）；
3. svcd 下发 `svcd_session` / `svcd_api` —— cookie 按主机存、不按端口，
   iframe 里的控制台直接就是登录态。

厂商会话绑客户端 IP，svcd **不能在服务端代查**（同一个 SID 从 PC 请求返回 SUCC，
从设备本机请求返回 404），所以“是否已登录”只能由浏览器里的页面判断。

免登录那扇门开不开**归宿主**：`svcd.conf` 的 `vendor_login = on`（默认 off，
plugin-manager ADR-0032）。本插件不读也不写宿主的配置；开着时能访问 `:8787` 的
非浏览器客户端可以伪造 `Origin` + `X-Vendor-OK` 拿到管理员会话 —— 只在“厂商后台是
这台设备唯一管理入口”的镜像上开；不开时控制台照常要求 svcd 密码。

> 老 plugin 服务（`sr6110_rebuild` 的 `autostart/plugin`）用的是设备上的
> `/etc/sr/web/plugins/menu.js`，那是迁移期的另一条路径。本插件不读它：包内
> `payload/menu.js` 才是新宿主的来源，两边内容需要保持一致。

## 构建与安装

```sh
plugin-tools build --dir plugins/webbind
# -> plugins/webbind/dist/webbind-1.0.0-aarch64-unknown-linux-musl-sr6110.spk
plugin install <spk>            # 目标机 allow_insecure_install = on（未签名开发包）
plugin start webbind
```

设备上的前置条件：

- `svc compat init --device sr6110` 写过一次 `<配置目录>/compat.conf`：本插件声明了
  `compat.devices: ["sr6110"]`，设备 id 不匹配时宿主不会加载它（和 sshfs、wireguard 的
  sr6110 包一样）；
- 厂商 httpd 的 `/home/httpd/public/jquery/static/js/app.*.js` 在（`/home` 是软链也认）。

## 测试

```sh
sh plugins/webbind/tests/webbind.sh
```

用假 `mount`/`umount` 和假挂载表，不需要 root、不碰真的 `/proc/mounts`；假挂载表的源
字段照真机写底层设备（不是 `mount --bind` 的源路径）。覆盖：无配置/路径写死、菜单注入
幂等、按目标卸载、别人的挂载不动、bind 失败、没有原厂 app.js、插件目录之外不写文件、
看护补挂。

## 从旧版本升级

旧版的覆盖副本放在 `/etc/sr/web/httpd/`，还会额外 bind 插件页目录
`/etc/sr/web/plugins` → `/public/plugins`。新版**只用插件自己的 `data/httpd/`**，
也不再碰插件页目录；升级后把旧残留清一次（只是文件，删掉不影响）：

```sh
rm -rf /etc/sr/web/httpd
grep -E 'public/(plugins|jquery/static/js)' /proc/mounts   # 有输出就 umount 掉对应挂载
```

升级后重新 `svc restart webbind`，覆盖副本会重新生成在插件目录里：

```sh
ls "<插件目录>/data/httpd/jquery/static/js/"     # app.<hash>.js，内容 = 原厂 app.js + menu.js
svc webbind status                               # overrides/overrides_bound 应为 1
```

## 排障

| 症状 | 看哪里 |
|---|---|
| `stop` 之后菜单还在 | `svc logs webbind` 里搜 `unbound` / `unmount failed`；`grep 'public/jquery/static/js' /proc/mounts` 看目标路径长什么样（真机上 `/home` 已解析成 `/kmodule/home`） |
| 菜单不出现 | `svc webbind status`：`menu` false 说明包内 `payload/menu.js` 没装进去；`overrides_bound` 为 0 说明没挂上 |
| 打开控制台还要登录 | 宿主没开 `vendor_login = on`（`svcd.conf`，改完重启 svcd），或 svcd 版本不带 `/vendor-login`（plugin-manager ADR-0032） |
| 原厂后台升级后菜单消失 | `svc restart webbind`：按新的 `app.<hash>.js` 重新生成覆盖 |
