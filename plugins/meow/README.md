# meow — mihomo 兼容代理内核

把 [meow-rs](https://github.com/madeye/meow-rs) 编译成 svcd 插件。插件没有
JSON-RPC 能力；生命周期是 `start` / `stop` / `status`，启动后由 meow 自己
提供 mixed 代理端口和 YACD 面板。

同一份源码支持 Linux、Windows 11 和 macOS。入口的家族差异写在
`platforms/unix.json` / `platforms/windows.json` 里：Linux/macOS 用 `meow.sh`（POSIX），
Windows 用 `meow.ps1`（PowerShell），打包时合并进清单，见下面的「入口」。

## 平台与形态

透明代理的形态由配置里的顶层键 `proxy-mode` 选（缺省 `pc`）：

| 平台 | `pc`：本机透明代理 | `router`：网关（LAN） |
| --- | --- | --- |
| Linux | `tproxy-port` → meow 自己装 nft `output` 链（root + nftables） | 插件用 iptables nat `prerouting` 劫持 LAN 转发（root + iptables，**只有 iptables 的内核也能用**） |
| Windows 11 | `tun:` → Wintun 适配器 + fake-ip（管理员权限） | 不支持（没有 iptables） |
| macOS | `tproxy-port` → pf（实验性，只拦 loopback） | 不支持（插件不装 pf 网关规则） |

`proxy-mode` 是插件自己的键，meow 不认识它、也不会用它。路由器那类固件内核常常
`# CONFIG_NF_TABLES is not set`、也没有 `nft` 和 `addrtype` 匹配，meow 内置防火墙
在 listener 启动阶段就会失败，所以网关只能走 `router`；Windows 上反过来，本机
透明代理只有 TUN 一条路。

两种形态都要求足够的权限：Linux 的 nft/iptables 要 root，Windows 的 Wintun 要
管理员。

## 入口：清单只写这个包是什么，入口按平台挑

`.spk` 从来不是跨平台的：打包器给每个包盖 `platform`，所以「跨平台」是源码级的
事实，manifest 是包级的产物。宿主的 manifest 又只有**一套** `exec` / `lifecycle` /
`interpreter`，而它们指的是脚本：POSIX 的要 `sh`，PowerShell 的要 `powershell`，
一个值不可能同时是两者。

所以 `package/manifest.json` 只留每个平台都成立的东西（身份、元数据、UI、config、
能力），入口按家族写在 `platforms/` 里：

```
platforms/meow.sh            生命周期脚本（unix 家族）
platforms/meow.ps1           生命周期脚本（windows 家族）
platforms/meow-fw.sh         网关形态的 nat 规则（unix 家族）
platforms/unix.json          unix 家族的清单差异：interpreter + exec + lifecycle
platforms/windows.json       windows 家族的清单差异
```

7 个平台只有 2 份覆盖：unix 的 6 个三元组共用 `unix.json`（`sh` + `meow.sh`），
Windows 用 `windows.json`（`powershell` + `meow.ps1`）。合并顺序是
`package/manifest.json` → `platforms/<家族>.json` → `platforms/<三元组>.json`（本例没有
第三层，7 个平台谁都不需要例外）：同名顶层键整段替换（`lifecycle` 是一个键），`id` /
`version` 不许改。

脚本本身**不在** `package/` 里：`plugin-tools build --platform X` 先把覆盖并入清单，
再把 `platforms/` 下属于 X 所在家族的文件复制进 `payload/bin/`（`.sh` → unix，
`.ps1` → windows），然后才打包。于是 Linux 包里只有 `meow.sh` / `meow-fw.sh`，
Windows 包里只有 `meow.ps1`，另一侧从头到尾没进过包。

Windows 上宿主于是这样起：

```
powershell -NoProfile -ExecutionPolicy Bypass -File payload\bin\meow.ps1 start
```

没有原生启动壳：逻辑就在两份脚本里，`meow.sh` 管 Linux 的 nft/iptables，
`meow.ps1` 管 Windows 的 TUN。

## 布局

```
vendor/meow-rs/                         源码
platforms/meow.sh                       unix 生命周期（构建时进 payload/bin/）
platforms/meow.ps1                      windows 生命周期（构建时进 payload\bin\）
platforms/meow-fw.sh                    router 形态的 iptables 规则（unix）
platforms/unix.json                     unix 家族的清单差异（sh + meow.sh）
platforms/windows.json                  windows 家族的清单差异（powershell + meow.ps1）
build.json                              cargo 构建配方
build.sh                                平台映射与产物复制（内核二进制）
package/
├── manifest.json                       平台无关的清单：身份、元数据、UI、config、能力
├── config/config                       我们的开关（安装后是 plugins/meow/config）
└── payload/
    ├── config.yaml                     meow 配置模板（首次启动生成 plugins/meow/meow.yaml）
    ├── bin/                            只有内核二进制（构建产物 + 合入的脚本）
    ├── web/index.html                  控制台「打开」入口（跳转到 9090/ui/）
    ├── cn_domains.yaml                 rule-provider
    └── yacd/                           面板静态文件
```

装好之后插件目录里是两个文件：`config`（你的开关）和 `meow.yaml`（meow 的订阅与规则）。

打包前会清掉别的平台留下的同名产物（`meow` ↔ `meow.exe`），所以每个平台的包
里只有自己那份内核。

## 运行方式

- `mixed-port: 7890`：HTTP/SOCKS5 混合代理（`pc` 形态下浏览器也可以直接用它）。
- `external-controller: 127.0.0.1:9090`，YACD 在 `http://127.0.0.1:9090/ui/`。
- svcd 控制台的「打开」入口由 `manifest.ui` + `payload/web/index.html` 提供；
  页面会自动跳到 `http://<当前主机>:9090/ui/`。
- 默认只做 `DIRECT` 直出；在配置里加 `proxies` 并把 `rules` 的 `MATCH`
  改到代理组后即可用面板切换。

宿主的 `lifecycle.start` 是前台进程，所以 `pc` 形态直接 `exec meow`；`router`
形态要先起进程、等 tproxy 端口监听、再挂规则，所以它把 meow 放到后台并
`wait`，直到进程退出才返回。启动前都会先跑 `meow -t` 校验配置，失败就返回
非零而不是秒退。`stop` 按 pidfile 结束进程，`status` 输出一行 JSON。

## 配置

两个文件，各管各的：

| 文件 | 谁改 | 放什么 |
| --- | --- | --- |
| `<插件目录>/config` | 你（`plugin editconfig meow`） | 插件开关：`proxy-mode`、`mixed-port`、`tproxy-port`、`routing-mark`、`lan-interface`、`dns-port` |
| `<插件目录>/meow.yaml` | meow（订阅刷新、面板保存） | 订阅、代理组、规则、面板设置 |

meow **不读** `config`，所以这里的开关永远不会被它重写掉；反过来，`meow.yaml` 里那几个
受管键（`mixed-port`、`tun`、`dns`、`tproxy-port`）由插件在每次 `start` 时按形态写好，
你不需要填，填错了也会被纠正（其它内容原样保留，注释也不动）。

- 第一次启动时 `meow.yaml` 从包里的 `payload/config.yaml` 生成（`@DATA@` 渲染成数据目录）；
- 旧布局（订阅写在 `config` 里）第一次启动会自动迁到 `meow.yaml`，`config` 变回开关文件；
- 想把 `meow.yaml` 恢复出厂：删掉它再 `plugin restart meow`。

### proxy-mode：填一个键，其余自动

```yaml
proxy-mode: pc          # pc | router（Windows 只认 pc）
mixed-port: 7890        # HTTP + SOCKS 同一个口
tproxy-port: 7893       # Linux 的 pc / router 用；Windows 走 TUN
routing-mark: 9527      # Linux pc 的出向 SO_MARK
lan-interface: br0      # router：被劫持的 LAN 网口
dns-port: 1053          # router：LAN DNS 劫持的目标端口
```

| 形态 | 平台 | 插件自动写进 `meow.yaml` |
| --- | --- | --- |
| `pc` | Windows | `tun.enable` / `auto-route` / `dns-hijack: [any:53]` + `dns.enhanced-mode: fake-ip` + `fake-ip-range`（TUN 和 fake-ip 必须成对，缺一个 meow 一条路由都不装） |
| `pc` | Linux / macOS | `tproxy-port` + `routing-mark`，并关掉 `tun` |
| `router` | Linux | 关 `tun`，`dns.listen: 0.0.0.0:<dns-port>`，`listeners:` 里没有 tproxy 入口时补一条（`firewall: false`） |

切形态只改 `config` 里的 `proxy-mode` 再 `plugin restart meow`。从 `router` 切回 `pc`
时 `stop` 会先把残留的 nat 规则撤掉，不会留一个指向死端口的劫持。

## 本机透明代理（proxy-mode: pc）

### Linux / macOS

不需要 `meow-fw.sh`：meow 见到 `tproxy-port` 就自己装 nft `output` 链（macOS 是
pf），退出时自己拆（RAII，崩溃残留的表下次启动会被清掉）。插件会自动把
`tproxy-port` / `routing-mark` 写进 `meow.yaml`，要换端口就改 `config`：

```yaml
# <插件目录>/config
tproxy-port: 7893
routing-mark: 9527     # meow 自己出向的 SO_MARK，避免被自己的规则再抓一次
```

- 进程要以 root 跑（nft/pf 需要）。权限不够时 `meow` 会直接以错误退出，事件日志里
  能看到它给出的 `firewall: false` 建议。
- 只劫持**本机 TCP**。UDP 不劫持；要全量接管 UDP/IPv6 用 `tun:`。
- 上游代理服务器的 IP 由 meow 自动放行，不用手写 bypass。
- TProxy 只把 IP 交给规则引擎，按域名分流要靠 `sniffer`：

  ```yaml
  sniffer:
    enable: true
    sniff:
      TLS: { ports: [443] }
      HTTP: { ports: [80] }
  ```

  要用 fake-ip 的话，meow **不会**替你劫持本机 `:53`：把 `dns.enhanced-mode`
  设成 `fake-ip`，再把系统 DNS 指到 `dns.listen`（默认 `127.0.0.1:5353`）。
- 验证：`nft list tables | grep meow_tproxy` 有表、`curl -sS https://example.com`
  能通，就说明规则和数据面都在。
- 只有 iptables 的内核（很多路由器）走不了这条路——那种机器用 `router` 形态；
  `pc` 形态依赖 meow 自带的 nft/pf 托管。

### Windows 11

Windows 上没有 nftables/pf，本机透明代理只有 **TUN（Wintun）** 一条路。`config` 里
`proxy-mode: pc` 就够了，插件每次 `start` 都把这几行写进 `meow.yaml`（不用手填）：

```yaml
# <插件目录>/meow.yaml —— 插件自动维护这几行
tun:
  enable: true
  auto-route: true
  dns-hijack: [any:53]
dns:
  enable: true
  enhanced-mode: fake-ip          # 必须成对：缺 fake-ip 时 meow 一条路由都不装
  fake-ip-range: 198.18.0.1/16
```

- 宿主进程要以管理员运行（Wintun 适配器要提权）。没提权时插件会在日志里提示，
  meow 自己也会失败退出。
- `mixed-port` 也只写在 `config` 里，插件把它带到 `meow.yaml`。
- 订阅里带进来的 `tproxy-port` / `routing-mark` 会被删掉：那是 Linux nftables / pf 的路。
- `wintun.dll` 不用手动放：meow 会优先找 exe 旁边的，找不到就释放内置的一份。
- **`stop` / 异常退出后不会把电脑断网**：TUN 适配器是持久的，被强杀的 meow 来不及撤
  `auto-route` 装的路由，路由留着就会把整机流量导进一个没人读的网卡。插件在
  `stop` 之后、以及每次 `start` 之前都会把 `meow-tun*` 上的路由清干净。
- 只捕获 fake-ip 网段（域名流量）；直连 IP 字面量不走 TUN，这是 meow 在 Windows
  上的既有边界（见 `vendor/meow-rs/docs/tun.md`）。
- 验证：`Get-NetRoute -InterfaceAlias 'meow-tun'` 能看到 `0.0.0.0/1` 和
  `128.0.0.0/1`，`curl.exe -x http://127.0.0.1:7890 -I https://www.google.com` 能通。

## 网关形态（proxy-mode: router，仅 Linux）

设备本身是网关：插件按「先等监听、再挂规则 / 先撤规则、再停进程」的顺序管理
iptables nat 规则，进程意外退出也会撤规则（宁可直接连，也不留黑洞）。

`config` 里写这几行：

```yaml
proxy-mode: router
lan-interface: br0           # 被劫持的 LAN 网口
tproxy-port: 7893
dns-port: 1053               # LAN 的 DNS 劫持到这里
routing-mark: 9527
mixed-port: null             # 网关不需要 LAN 侧 HTTP/SOCKS
```

插件会把 `dns.enable` / `dns.listen: 0.0.0.0:<dns-port>`、`tun: false` 写进 `meow.yaml`，
没有 tproxy 入口时还会在 `listeners:` 下面补一条（`listen: 0.0.0.0`、`firewall: false`）。
剩下这几项留在 `meow.yaml` 里按需写：

```yaml
# <插件目录>/meow.yaml
ipv6: false                  # 规则只做 v4 REDIRECT；开 v6 会让 AAAA 绕过代理
mode: rule
allow-lan: true
bind-address: 0.0.0.0
external-controller: 0.0.0.0:9090
secret: "换成你自己的"

dns:
  enhanced-mode: fake-ip
  fake-ip-range: 198.18.0.1/16
  nameserver: [223.5.5.5, 119.29.29.29]
  fallback: [https://dns.google/dns-query, https://1.1.1.1/dns-query]
  fake-ip-filter: ["*.lan", "*.local"]
```

- 顶层 `tproxy-port` 在 router 形态会被插件删掉：那是 `pc` 形态的托管入口，会和
  `listeners` 抢端口。
- `routing-mark` 省略时只是不装 SO_MARK 放行规则；`dns.listen` 绑 loopback 则直接启动失败。
- 规则内容（`meow-fw.sh up`）：
  1. `MEOW_DNS`：LAN 的 `:53` udp/tcp → `REDIRECT` 到 `dns.listen` 的端口；
  2. `MEOW_TPROXY`：命中 `routing-mark` → RETURN；保留网段 → RETURN；其余 TCP
     → `REDIRECT` 到 tproxy 端口；
  3. 两条 `PREROUTING -i <lan-interface>` 跳转，DNS 在 tproxy 之前。
- 为什么规则不用 meow 自带的：固件内核没有 nf_tables / nft；为什么不用
  `addrtype`：内核没编 `xt_ADDRTYPE`。所以这里用 iptables 写，语义与 meow
  内置的托管规则一致（见 `vendor/meow-rs/scripts/tproxy-gateway-linux.sh`）。

## 调试

```bash
# Linux/macOS：只读配置，不动 meow 进程
sh <插件目录>/<版本>/payload/bin/meow.sh mode        # 打印解析出来的形态/端口/网口
sh <插件目录>/<版本>/payload/bin/meow.sh fw status   # 看规则在不在
sh <插件目录>/<版本>/payload/bin/meow.sh fw down     # 手动撤规则

# Windows
powershell -NoProfile -ExecutionPolicy Bypass -File <插件目录>\<版本>\payload\bin\meow.ps1 mode
```

## 构建

```bash
plugin-tools build --platform x86_64-unknown-linux-gnu
plugin-tools check dist/meow-1.2.0-x86_64-unknown-linux-gnu.spk
```

Rust 1.98.1（vendor 里的 `rust-toolchain.toml`）和 cmake/C++ 是构建依赖；
BoringSSL 由 meow-rs 自己静态链接。构建只产出一个二进制（meow 内核），入口是
`payload/bin/` 里的两个脚本。

本机构建要满足三件事，缺哪件 `build.sh` 都会先说清楚再退出：

| 要的东西 | 说明 |
| --- | --- |
| 目标装到 pin 的工具链上 | `rustup target add --toolchain 1.98.1-<本机三元组> <目标>`。装到默认工具链不算数——`rust-toolchain.toml` 把版本固定在 1.98.1 |
| cmake | BoringSSL 用它构建；Windows 上 `scoop install cmake`，Linux 上 `apt install cmake` |
| 交叉 C 工具链 | musl 目标需要（`X86_64_MUSL_CROSS` / `AARCH64_CROSS` / `CROSS_PREFIX`）；`build.sh` 会探测 `/usr/soft/*-cross` 和 `/d/soft/x86_64-linux-musl`，找不到就报错 |

`build.sh` 会先写一份 CMake 工具链文件把 `BUILD_TESTING` 关掉：BoringSSL 缺省会把
google benchmark 一起配进来，而它的特性探测在 Windows 上很容易失败（
`Failed to determine the source files for the regular expression backend`），交叉编译时
也会卡在它的 `find_package(Threads)`。我们只要 libcrypto/libssl。

只打本机那一个平台：`plugin-tools build --platform <本机三元组>`。不带 `--platform`
会按 `.plugin.json` 的 7 个平台逐个交叉编译，本机通常编不了 darwin，交叉 musl 也要上面
那套工具链。

发布流水线按 `.plugin.json` 的 7 个平台构建；Linux musl 三个目标用
musl cross 工具链编译 BoringSSL，Windows 目标在 windows runner 上编译
（`wintun.dll` 由 meow 在编译期取回并内嵌），macOS 目标用宿主 Xcode 工具链。
build.json 的命令是 `sh build.sh`，所以 Windows runner 上 CI 会先把 Git 的
`bin` / `usr/bin` 加进 PATH。

发布 job 打 tar 的不是 `package/`，而是 `--stage` 指的那个目录（`dist/stage`）：
Windows 那份合并过的清单得先进包，签名 job 重新 bundle 时才有 `meow.ps1`。

本地在 Windows 上构建时，打包器的 stale-twin 逻辑会顺手删掉
`package/payload/bin/meow`（它现在是构建产物，不再进仓库），换个平台重编就回来了。
