# wireguard — WireGuard 隧道

把旧固件里 `autostart/10-wireguard` + `sh/wg-start.sh` + `sh/wg-lan-fix.sh` 合成一个
svcd 插件。隧道、密钥、peer 和管理页面都在插件里，生命周期交给宿主。

**一个插件，两种后端，按包里的二进制自动选：**

| 包 | 后端 | 接口怎么来 | 适用 |
|---|---|---|---|
| `…-aarch64-unknown-linux-musl-sr6110.spk` | 内核模块 | 包内 `wireguard.ko` + `ip link add type wireguard` | SR6110（5.4.196 内核 + ABI 补丁） |
| 其余平台 | gotatun（用户态） | 包内 `gotatun` 建 TUN | 任何有 `/dev/net/tun` 的 Linux |

两套用的是同一份 `wireguard.sh` / `wireguardd.sh` / 管理页面：`WG_IMPL=auto` 时包里
有 `wireguard.ko` 就走内核，否则用 `gotatun`；`data/settings` 里可以强制指定。

当前支持并出包的三元组：`x86_64-unknown-linux-musl`、`aarch64-unknown-linux-musl`、
`armv7-unknown-linux-musleabihf`、`x86_64-unknown-linux-gnu`，外加 `sr6110` 变体。
四个平台各自从源码编自己的二进制：`gotatun` 是静态 musl/gnu，`wg` 统一静态链接，
所以插件不依赖宿主的 libc 版本。

macOS / Windows 还没做：它们要换网络层（`ifconfig`/`route`/`pf`、`netsh`/WinTun）和
入口（PowerShell），不是换一个二进制的事。

## 产物

```
wireguard-1.2.1-x86_64-unknown-linux-musl.spk
wireguard-1.2.1-aarch64-unknown-linux-musl.spk
wireguard-1.2.1-armv7-unknown-linux-musleabihf.spk
wireguard-1.2.1-x86_64-unknown-linux-gnu.spk
wireguard-1.2.1-aarch64-unknown-linux-musl-sr6110.spk   ← compat.devices: ["sr6110"]
```

设备后缀来自该包 manifest 的 `compat.devices[0]`。通用包没有 `compat`，任何设备都能装；
`sr6110` 变体只装到兼容标识为 `sr6110` 的设备。

## 布局

```
.plugin.json              platforms + variants（sr6110 变体只在 aarch64 上多出一个包）
build.json                构建配方（artifact: out/{platform}/wg-tool）
build.sh                  打包前编译：从 vendor/ 源码编 wg 和 gotatun；sr6110 只拷预编译 ko
package/
├── manifest.json         平台无关的清单（没有 compat，变体由构建注入）
└── payload/
    ├── bin/
    │   ├── wireguard.sh  生命周期：prepare / up / down / status
    │   ├── wireguardd.sh 被监督进程：启动时建隧道 + 前台看守；能力钩子也是它（hook 子命令）
    │   ├── wg-lan-fix.sh 内核后端的 LAN 自愈循环（sr6110 专用）
    │   ├── wg-tool       构建产物（wg）
    │   └── wireguard.ko  或 gotatun：构建产物，按包选一个
    ├── lib/
    │   ├── svcd-plugin.sh    协议壳（sdk/shell 副本）
    │   └── wireguard-lib.sh  设置/密钥/peer/后端判定
    └── web/                  管理页面（静态，无构建步骤）
vendor/gotatun-0.9.2/     gotatun 源码（MPL-2.0），打包前编译
vendor/wireguard-tools/   wg 源码（GPL-2.0），打包前编译
vendor/prebuilt/…/wireguard.ko  SR6110 内核模块：唯一预编译件（要对着厂商内核树编）
```

## 运行方式

| 动作 | 做什么 |
|---|---|
| `lifecycle.start`（`wireguard.sh prepare`） | 生成服务端密钥、迁移旧配置、等 LAN 口拿到 IPv4、解析 `WG_LAN_IF=auto`。必须退出，不建接口。 |
| `exec`（`wireguardd.sh start`） | **被监督进程**：启动时 `wireguard.sh up` 把隧道拉起来（gotatun 因此是它的子进程，宿主停服按进程组收走），然后在前台看守。没有协议通道（`exec.ready` 是 `exec`）。 |
| 能力钩子（`wireguardd.sh hook <能力>`） | 管理页面的 `state` / `save_settings` / `add_peer` / `remove_peer` / `peer_config`：宿主每次 `tools/call` 起一次本脚本，`arguments` 从 stdin 读，stdout 就是这次调用的文本结果（svcd #342）。 |
| 看守循环 | 内核后端：跑 `wg-lan-fix.sh`（厂商固件会刷回 `rp_filter` / 策略路由）；用户态后端：`gotatun` 死了就重建。 |
| `lifecycle.stop` | 进程消失**之后**收尾：撤规则、清 `ip rule` / table 200、停 gotatun、`ip link del wg0`。 |
| `lifecycle.status` | 一行 JSON（`running` / `impl` / `listen_port` / `peers` / `lan`），给 `svc status` 的 probe 用。 |

密钥全部在插件目录内、首次启动自动生成：

| 文件 | 用途 |
|---|---|
| `data/wg/server.key` / `server.pub` | 服务端身份；`server.pub` 显示在管理页，也写进每个客户端配置 |
| `data/peers.tsv` | peer 列表（含客户端私钥，0600）；每行 `id 名称 隧道地址 公钥 私钥 创建时间` |
| `data/settings` | 管理页面保存的隧道设置（0600，shell 可 source 的 `KEY=value`），含 `WG_IMPL` / `WG_LAN_FIX` |
| `data/gotatun.pid` | 用户态后端的进程号；`stop` 时校验命令行后再杀（防 pid 复用） |
| `data/wireguardd.pid` | 看守（被监督进程）的进程号；能力钩子是另一个进程，靠它回 `status.guardian`（同样校验命令行） |

日志只写 stderr，宿主落到自己的日志目录（Linux 默认 `/var/log/svcd/wireguard.log`）。
`gotatun` 的 stdout 也改道 stderr——stdout 是宿主收日志的地方（没有协议通道，能力走钩子）。

## 管理页面

1. **隧道设置**：内网接口、监听端口、隧道网段、服务端地址、MTU、策略路由表号、
   客户端连接地址、DNS、AllowedIPs、PersistentKeepalive。涉及接口 / 端口 / 网段 /
   地址 / MTU 的改动会重建隧道（连接短暂断开）。
2. **多个 peer**：填名字添加，自动分配 `10.99.0.x` 地址、生成客户端密钥并应用。
   每行可以查看二维码 / 客户端配置、删除。
3. **二维码**：客户端配置含 `PrivateKey`、`Address`、可选 `DNS`，以及服务端的
   `PublicKey`、`Endpoint`、`AllowedIPs`、`PersistentKeepalive`。
4. **状态**：运行中会显示当前后端（`内核模块` / `gotatun`）和活动 peer 数。

约束与提示：

- 隧道网段目前只支持 `/24`（peer 地址按最后一段顺序分配）。
- `内网接口` 默认 `auto`：br0 → br-lan → 任意带 IPv4 的网桥 → 默认路由口；
  解析不到再退回 br0。也可以直接填接口名。
- `客户端连接地址` 填域名/IP 时自动补监听端口，也可以写 `host:port`；IPv6 写 `[addr]:port`。
- 已经有 peer 时不允许改隧道网段；先删 peer 再改。
- 服务端地址不能和已分配的 peer 地址重复。
- 删除 peer 会同时从内核/UAPI 与 `peers.tsv` 里移除。
- 改监听端口或连接地址不会改写已经导出的客户端配置；重新打开该 peer 的二维码即可。
- 页面里的 peer 私钥通过 capability 返回给页面用于生成二维码；peer 列表只显示公钥。

## 安装

通用包直接装（`compat` 为空，任何设备可装）：

```sh
plugin install wireguard-1.2.1-aarch64-unknown-linux-musl.spk
plugin start wireguard
```

SR6110 用变体包，并且要先写设备兼容标识（只做一次）：

```sh
svc compat init --device sr6110
plugin install wireguard-1.2.1-aarch64-unknown-linux-musl-sr6110.spk
plugin start wireguard
```

打开管理页面，先填“客户端连接地址”（客户端要连的公网域名/IP），保存；然后添加
peer、扫码。

**和旧服务不要同时开**：旧固件的 `svc wireguard start` 也会建 `wg0` 和同一组规则。
切换时先 `svc wireguard stop && svc wireguard disable`，再启动插件。

## 构建

和 `meow` / `sshd` 一样，**打包前从 `vendor/` 源码编译**，只有 SR6110 的内核模块是预编译件。

```bash
# 只打本机三元组的通用包（需要该三元组的 C/Rust 工具链）
plugin-tools build --platform x86_64-unknown-linux-musl

# 只打 sr6110 变体（aarch64 musl；ko 直接拷 vendor/prebuilt）
plugin-tools build --device sr6110 --platform aarch64-unknown-linux-musl

# 一次打出全部 5 个包
plugin-tools build
```

`--platform` 不带 `--device` 时只打该平台的通用包；变体必须显式用 `--device` 指定，
产物名才会带 `-sr6110`。

两个包都要先把 `wg`（和通用包的 `gotatun`）从源码编出来，工具链走仓库那张唯一的表
`tools/cross-env.sh`（`build.sh` 里 `svcd_cross_apply`），目标机器要提前装好
`rustup target` 和对应的 musl 交叉 gcc（`tools/cross/install.sh <triple>` 会装到
`/usr/soft`）。`gotatun` 的 `aws-lc-rs` 还需要 `cmake` / `clang` / `perl`。

不想在宿主装工具链：有 docker 的 WSL/Linux 上用 `tools/pack.sh wireguard all`（含变体），
它给 `build.sh` 的是 `SVCD_DOCKER=1`，编译全在 rust-cross 镜像里做。CI 也是从源码编。

## 后端与自愈的边界

- 后端由**包里带了什么**决定：`wireguard.ko`（sr6110 变体）走内核，`gotatun`（通用包）
  走用户态。不探测宿主是否碰巧加载了 wireguard 模块——同一台机器上行为不该漂移。
- `WG_LAN_FIX=auto`：只有**内核后端**跑 `wg-lan-fix.sh`。那段逻辑是给厂商固件的三个
  坑写的（网桥数据通路、`rp_filter`、策略路由被刷回），用户态后端不需要。
- `WG_LAN_FIX=on/off` 可强制；`WG_IMPL=kernel/userspace` 可强制后端（排障用）。
- 两个值都在 `data/settings` 里，管理页面不暴露；改完重启插件生效。

## 从 1.0 / 1.1 升级

- 1.0 的 `<插件目录>/config` 只在还没有 `data/settings` 时读一次；之后只认管理页面。
- 1.0 单套对端密钥 `data/wg/peer.key` / `peer.pub` 会自动迁成 `peers.tsv` 的一条
  “旧客户端”，地址按新网段分配。
- 1.1 的 `data/settings` 没有 `WG_IMPL` / `WG_LAN_FIX`，升级后按默认 `auto` 处理；
  SR6110 上仍走内核模块。

## 已知取舍

- 密钥首次启动自动生成；删掉 `data/wg` 会换服务端密钥，所有客户端要重新扫码。
- 内核模块加载后不 `rmmod`：可能被别的使用者共享，卸载只删 `wg0`。
- `lifecycle.stop` 会清空 `WG_TABLE` 里的路由。这个表号归本插件使用。
- 规则用 iptables 而不是 nft（厂商内核 `# CONFIG_NF_TABLES is not set`）；用户态
  后端在没有 iptables 的桌面上只打日志，不影响隧道本身。
- 二维码库是 qrcode-generator 1.4.4（MIT），随包放在 `payload/web/qrcode.js`。
- `gotatun` 需要 `/dev/net/tun`；内核没编 TUN 的机器会在启动时报错而不是静默失败。
