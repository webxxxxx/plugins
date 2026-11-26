# meow — mihomo 兼容代理内核

把 [meow-rs](https://github.com/madeye/meow-rs) 编译成 svcd 插件。插件没有
JSON-RPC 能力；生命周期是 `start` / `stop` / `status`，启动后由 meow 自己
提供 mixed 代理端口和 YACD 面板。

同一份源码支持 Linux、Windows 11 和 macOS。入口的家族差异写在
`platforms/unix.json` / `platforms/windows.json` 里：Linux/macOS 用 `meow.sh`（POSIX），
Windows 用 `meow.ps1`（PowerShell），打包时合并进清单，见下面的「入口」。

## 平台与形态

透明代理的形态由配置里的顶层键 `proxy-mode` 选（缺省 `off`）；页面顶部的代理类型
`off` / `tun` / `tproxy` 就是它的三个取值 `off` / `pc` / `router`：

| 平台 | `off`：不接管 | `pc`：本机透明代理 | `router`：网关（LAN） |
| --- | --- | --- | --- |
| Linux | 只开 `mixed-port`（HTTP/SOCKS） | `tproxy-port` → meow 自己装 nft `output` 链（root + nftables） | 插件用 iptables nat `prerouting` 劫持 LAN 转发（root + iptables，**只有 iptables 的内核也能用**） |
| Windows 11 | 只开 `mixed-port`，`tun.enable` 关掉 | `tun:` → Wintun 适配器 + fake-ip（管理员权限） | 不支持（没有 iptables） |
| macOS | 只开 `mixed-port` | `tproxy-port` → pf（实验性，只拦 loopback） | 不支持（插件不装 pf 网关规则） |

`proxy-mode` 是插件自己的键，meow 不认识它、也不会用它。路由器那类固件内核常常
`# CONFIG_NF_TABLES is not set`、也没有 `nft` 和 `addrtype` 匹配，meow 内置防火墙
在 listener 启动阶段就会失败，所以网关只能走 `router`；Windows 上反过来，本机
透明代理只有 TUN 一条路。

`off` 不启动 tun、也不开 tproxy：需要代理的软件自己指到 `mixed-port` 就行，系统流量
完全不动。`pc` / `router` 都要求足够的权限：Linux 的 nft/iptables 要 root，Windows 的
Wintun 要管理员。

**刚装好时就是 `off`**：`state` 里没有 `proxy-mode` 时缺省就是它，`start` 只开
`mixed-port`，不碰系统路由和 DNS。缺省不能是 `pc`——这时既没有订阅也没有可用的出口，
TUN / tproxy 一起就把整机流量导进一个转不出去的内核，表现就是断网。需要代理时在页面上
选一次 `tun` / `tproxy`，选择记进 `state`，升级和重启都还认。

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
└── payload/
    ├── config.yaml                     meow 的配置模板（安装后就是 <插件目录>/config）
    ├── bin/                            只有内核二进制（构建产物 + 合入的脚本）
    ├── web/index.html                  控制台「打开」入口：顶部代理类型 + 订阅管理，下面 iframe 嵌 9090/ui/
    ├── cn_domains.yaml                 rule-provider
    └── yacd/                           面板静态文件
```

装好之后插件目录里是两个文件：`config`（meow 的订阅与规则，`plugin editconfig meow` 编辑它）
和 `state`（插件自己的代理类型缓存）。

打包前会清掉别的平台留下的同名产物（`meow` ↔ `meow.exe`），所以每个平台的包
里只有自己那份内核。

`payload/yacd/` 是 YACD 的静态文件，其中 `sw.js` 会用 navigate 回退把 `/ui/` 的导航交给
预缓存的那份 `yacd/index.html`。改了外壳（比如主题同步用的 `theme-bridge.js`）就必须把
`sw.js` 里 `index.html` 的 `revision` 换成新文件的 md5，否则装到设备上、用户浏览器里
还是旧外壳，看着就像“没生效”。`tools/meow-config-test.sh` 有一步守着这条。

## 运行方式

- `mixed-port: 7890`：HTTP/SOCKS5 混合代理（浏览器可以直接用它，`off` 形态只有它）。
- `external-controller: 127.0.0.1:9090`，YACD 在 `http://127.0.0.1:9090/ui/`。
- svcd 控制台的「打开」入口是我们自己的页面（`payload/web/index.html`）：顶部是**代理类型**
  （`off` / `tun` / `tproxy`）和**订阅管理**按钮，下面用 iframe 嵌 meow 自带的 YACD；
  面板地址按当前浏览器访问的
  主机名拼（`http://<当前主机>:9090/ui/`），`127.0.0.1` / `localhost` / 局域网地址都能用。
- 页面上的选择立刻生效，而且是**持久**的：它写进 `<插件目录>/state` 的 `proxy-mode`，
  插件关掉再开也按它走。
- 顶部还有一个**订阅管理**按钮：点开是一个弹层（`<dialog>`），里面是 meow 自己那份
  `/ui/sub.html`，添加 / 刷新 / 删除订阅走的还是 `/api/subscriptions`。弹层用右上角的
  `×`、Esc 或点弹层外面都能关。YACD 里那个浮动订阅按钮已经去掉——入口只在页面顶上。
  弹层外壳和订阅页也跟着主题换：打开时用 `?theme=` 把当前主题带过去，弹层开着时主题
  变了再用 `postMessage` 同步（订阅页在 iframe 里，自己读不到外层的主题）。
- 顶部这一栏的深色 / 浅色跟着 **YACD 里切的主题**走，不跟系统偏好：YACD 把主题写在它
  自己的 `<html data-theme="dark|light|auto">` 上，`yacd/theme-bridge.js` 盯着这个属性，
  变了就用 `postMessage` 报到外层页面（两边不同源，读不到彼此的 DOM 和 localStorage），
  `auto` 按系统偏好折算。外层页面把最近一次的主题存在自己的 `localStorage` 里，刷新时
  先按它上色，等 iframe 报到再对齐。`/ui/sub.html` 单独打开时也一样认 `?theme=`，
  没有就跟系统偏好。
- 面板是 meow 自己在 `:9090` 上开的；如果宿主是用 https 打开的，浏览器会拦掉这个
  http iframe（控制台本身走 http 就没这个问题）。
- 默认只做 `DIRECT` 直出；在配置里加 `proxies` 并把 `rules` 的 `MATCH`
  改到代理组后即可用面板切换。

清单里 `exec.ready` 是 `notify`：宿主先发一条 `svcd.ping`，收到回复才算启动成功，
之后这条路才是能力通道（`ready: exec` 的 stdout 只进日志，页面调不到任何能力）。

宿主的 `exec` 就是那个**看护进程**（`exec.args` 是 `start`）：meow 是自己的子进程，
能力通道跑在后台（页面上的代理类型调的就是它）。`pc` / `off` 形态起进程就返回；`router`
形态还要等 tproxy 端口监听、挂好 iptables 规则。启动前会跑 `meow -t` 校验配置，失败就返回
非零而不是秒退。孩子自己退出时看护循环跟着退出（宿主据此重启或报错）；页面切了代理类型
就让孩子退出，看护循环读到新的 `proxy-mode` 后按新形态重启。`stop` 在进程退出之后跑：
宿主负责终止（Unix 进程组 SIGTERM → 10 秒 → SIGKILL；Windows 直接强杀整棵树），`stop`
只收尾——先撤 TProxy 规则，再删掉强杀留下的 pidfile，它不杀进程；`status` 输出一行 JSON。

## 配置

两个文件，各管各的：

| 文件 | 谁改 | 放什么 |
| --- | --- | --- |
| `<插件目录>/config` | 你（`plugin editconfig meow` / 直接编辑） | **meow 自己的配置**：订阅、代理组、规则、面板、端口……meow 就是用这一份启动的（`meow -f <插件目录>/config`） |
| `<插件目录>/state` | 插件（页面上的代理类型） | 只缓存 `proxy-mode`。是插件的状态，不是给你编辑的配置 |

`config` 里那几个**受管键**（`mixed-port`、`tun`、`dns`、`tproxy-port`、`routing-mark`）由插件
在每次 `start` 时按形态写好，你不需要填；其余内容（订阅、规则、注释）原样保留。
`mixed-port` 以你写的为准：插件只在你没写的时候补 `7890`。

- 第一次安装时 `config` 从包里的 `payload/config.yaml` 生成（`@DATA@` 渲染成数据目录）；
  老版本生成过的配置里如果还留着字面量 `@DATA@`，每次 `start` 都会就地换成真实数据目录
  （否则 meow 会拿 `@DATA@/Country.mmdb` 去找 GeoIP 库，加载失败后整份配置都起不来，
  面板添加订阅的接口会报 HTTP 400）；
- 包里带了一份精简的 `Country.mmdb`（约 130 KB），第一次启动会放进数据目录；数据目录里
  已有这个文件就不动它（可能是 meow 自己更新过的），别的 geo 库缺失时 meow 会自己下载；
- 从 1.2/1.3 升级：旧的 `<插件目录>/meow.yaml` 会被搬回 `config`，旧的 `proxy-mode` 搬进 `state`；
- 更老的布局（开关和订阅写在一起）也认，插件会把自己的键摘出来放进 `state`；
- 想把 `config` 恢复出厂：删掉它再 `plugin restart meow`。

### proxy-mode：填一个键，其余自动

页面顶上只有三个值：`off` / `tun` / `tproxy`，落盘永远是 `state` 里的 `proxy-mode`：

| 页面上的代理类型 | `proxy-mode` | 含义 |
| --- | --- | --- |
| `off` | `off` | 不启动 tun，也不开 tproxy：meow 只当普通 HTTP/SOCKS 代理（`mixed-port`） |
| `tun` | `pc` | 本机透明代理：Windows 走 TUN + fake-ip，Linux/macOS 走 `tproxy-port` |
| `tproxy` | `router` | 网关：iptables 劫持 LAN，只有 Linux 有（Windows 上页面会把这个选项标成「仅 Linux」） |

`<插件目录>/state` 里只有一行：

```yaml
proxy-mode: off         # off | pc | router（Windows 只认 off / pc）；刚装好的缺省值
```

`config` 里跟形态有关的键（默认值，插件按形态写）：

```yaml
mixed-port: 7890        # HTTP + SOCKS 同一个口，off 形态下只有它
tproxy-port: 7893       # Linux 的 pc 形态用；Windows 走 TUN
routing-mark: 9527      # Linux pc 的出向 SO_MARK
```

网关形态（`router`）还要两个插件自己的参数，默认 `br0` / `1053`，要改就写进 `state`：

```yaml
lan-interface: br-lan   # 被劫持的 LAN 网口
dns-port: 1053          # 劫持 LAN DNS 用的端口
```

| 形态 | 平台 | 插件自动写进 `config` |
| --- | --- | --- |
| `off` | 全部 | 关掉 `tun`，删掉 `tproxy-port` / `routing-mark`，只留 `mixed-port` |
| `pc` | Windows | `tun.enable` / `auto-route` / `dns-hijack: [any:53]` + `dns.enhanced-mode: fake-ip` + `fake-ip-range`（TUN 和 fake-ip 必须成对，缺一个 meow 一条路由都不装） |
| `pc` | Linux / macOS | `tproxy-port` + `routing-mark`，并关掉 `tun` |
| `router` | Linux | 关 `tun`，`dns.listen: 0.0.0.0:<dns-port>`，`listeners:` 里没有 tproxy 入口时补一条（`firewall: false`） |

切形态就一条路：页面上选一下（写 `state` → 重启 meow，几秒钟，插件本身不用重启）。
从 `router` 切回 `pc` 时 `stop` 会先把残留的 nat 规则撤掉，不会留一个指向死端口的劫持。

### 面板不用登录

订阅里常常自带一个 `secret`，YACD 于是每次弹登录框。页面的 `mode` 能力会把这个 `secret`
读出来，拼进 iframe 的 URL（`…:9090/ui/?secret=…`），所以面板打开就是登录状态。
订阅管理弹层同理：`/ui/sub.html?embed=1&secret=…`，第一次打开这个页面时也不用先在面板里
登录一遍。
想彻底关掉密钥：把 `config` 里的 `secret` 改成 `""` 再重启（控制器只监听 `127.0.0.1`）。

## 本机透明代理（proxy-mode: pc）

### Linux / macOS

不需要 `meow-fw.sh`：meow 见到 `tproxy-port` 就自己装 nft `output` 链（macOS 是
pf），退出时自己拆（RAII，崩溃残留的表下次启动会被清掉）。插件会自动把
`tproxy-port` / `routing-mark` 写进 `config`，要换端口就改 `config`：

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

Windows 上没有 nftables/pf，本机透明代理只有 **TUN（Wintun）** 一条路。页面上的
代理类型选 `tun`（也就是 `proxy-mode: pc`）就够了，插件每次 `start` 都把这几行写进
`config`（不用手填）：

```yaml
# <插件目录>/config —— 插件自动维护这几行
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
- 选 `off` 就把 `tun.enable` 关掉：不建 TUN 适配器、不动路由和 DNS，只有 `mixed-port` 能代理。
- `mixed-port` 以 `config` 里的为准，没写时插件补 `7890`。
- 订阅里带进来的 `tproxy-port` / `routing-mark` 会被删掉：那是 Linux nftables / pf 的路。
- `wintun.dll` 不用手动放：meow 会优先找 exe 旁边的，找不到就释放内置的一份。
- **`stop` / 异常退出后不会把电脑断网**：TUN 适配器是持久的，被强杀的 meow 来不及撤
  `auto-route` 装的路由和它改过的 DNS，两样都会把整机弄成“上不了网”。插件在 `stop`
  之后、以及每次 `start` 之前都会清 `meow-tun*` 上的路由，并把被改成 `127.0.0.1` / `::1`
  的网卡 DNS 恢复回去（`start` 前先存一份真实 DNS，恢复时先重置回 DHCP 再按备份写回）。
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

插件会把 `dns.enable` / `dns.listen: 0.0.0.0:<dns-port>`、`tun: false` 写进 `config`，
没有 tproxy 入口时还会在 `listeners:` 下面补一条（`listen: 0.0.0.0`、`firewall: false`）。
剩下这几项留在 `config` 里按需写：

```yaml
# <插件目录>/config
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

## 停止后上不了网（Windows）

两种残留，都来自“被强杀的进程来不及收拾”：

| 残留 | 症状 | 插件怎么处理 |
| --- | --- | --- |
| `meow-tun` 上的路由 | 有网卡、网页全超时 | `stop` 后 + 每次 `start` 前清掉 `meow-tun*` 的路由 |
| 网卡 DNS 被改成 `127.0.0.1` / `::1` | `ping 223.5.5.5` 通、打不开网页（解析失败） | `stop` 后 + 每次 `start` 前恢复（先重置回 DHCP，再按启动前存的备份写回） |

关机/任务管理器结束进程这类情况插件没有机会跑，下次 `plugin start meow` 会自愈；
已经卡住了想立刻救回来，管理员 PowerShell 里两条就够：

```powershell
foreach ($fam in 'IPv4','IPv6') {
  Get-DnsClientServerAddress -AddressFamily $fam |
    Where-Object { $_.ServerAddresses -and ($_.ServerAddresses -match '127\.0\.0\.1|::1') } |
    ForEach-Object { Set-DnsClientServerAddress -InterfaceAlias $_.InterfaceAlias -ResetServerAddresses }
}
Clear-DnsClientCache
```

（meow 的源码里这段逻辑是 `tun` 的 `DnsGuard`：设置时把所有网卡 DNS 改成 loopback，
正常退出时恢复；所以问题只出在强杀。）

## 调试

```bash
# Linux/macOS：只读配置，不动 meow 进程
sh <插件目录>/<版本>/payload/bin/meow.sh mode         # 打印解析出来的形态/端口/网口
sh <插件目录>/<版本>/payload/bin/meow.sh status       # 一行 JSON（running / mode / proxy / 端口）
sh <插件目录>/<版本>/payload/bin/meow.sh set-mode tun  # 和页面上的选择器同一条路：写 config 再重启孩子
sh <插件目录>/<版本>/payload/bin/meow.sh fw status    # 看规则在不在
sh <插件目录>/<版本>/payload/bin/meow.sh fw down      # 手动撤规则

# Windows
powershell -NoProfile -ExecutionPolicy Bypass -File <插件目录>\<版本>\payload\bin\meow.ps1 mode
powershell -NoProfile -ExecutionPolicy Bypass -File <插件目录>\<版本>\payload\bin\meow.ps1 status
```

页面上的代理类型调的是插件能力 `mode`（不带参数读，带 `{"mode":"tun"}` 写）；
`svc call meow.mode` 拿到的是同一份 JSON。status / mode 里的 `proxy` 字段就是页面上的标签。

## 构建

### 本机打包（推荐）：docker 交叉编译，只出 windows + linux

宿主机的 rust/cmake/MSVC/libclang 全都不用装，WSL 里 docker 一个就够了：

```sh
sh tools/pack.sh                                 # windows-msvc + linux-gnu 两个 .spk
sh tools/pack.sh meow x86_64-unknown-linux-musl  # 想多出 musl / arm 就加参数
```

**Linux 发哪个包**：`linux-gnu` 是在 Ubuntu 24.04（glibc 2.39）上链出来的，宿主机 glibc
比它老就直接跑不起来——`meow -t` 报 `version 'GLIBC_2.38' not found`，插件日志里只看得到
“配置校验失败”。WSL 里的 Debian 12（glibc 2.36）、多数路由器固件都要发 musl 包：

```sh
sh tools/pack.sh meow x86_64-unknown-linux-musl
```

musl 包要用**整份** musl 交叉工具链（musl.cc 那份，自带 musl 版 `libstdc++.a`）。镜像里的
`musl-gcc` 是 musl-tools 的 C 驱动，C++ 只能退到宿主机的 g++，那头是 glibc 的头和
libstdc++，链接期一定炸（`__isoc23_strtoull` / `fopen64` / `__libc_single_threaded`
未定义）。一次装好，之后连容器一起用（`/usr/soft` 挂进镜像）：

```sh
sh tools/cross/install.sh x86_64-unknown-linux-musl /usr/soft   # → /usr/soft/x86_64-linux-musl-cross
```

工具链装好了还是报 `fopen64` / `__isoc23_strtoull`（或者架构不对），是 BoringSSL 的 cmake
目录里留着上一次的宿主编译器和 `.o`。`build.sh` 现在把编译器用 `CACHE … FORCE` 写进工具链
文件（boring-sys 看到 `CMAKE_TOOLCHAIN_FILE` 就不再自己给编译器，只能由这里给全），重跑会
自己改过来；保险起见先删掉旧目录：

```sh
rm -rf ${SVCD_BUILD_WORKSPACE:-/usr/soft/build_workspace}/meow/target/x86_64-unknown-linux-musl/release/build/{boring-sys,quiche}-*
```

插件启动时会先跑一次 `meow -t` 校验配置；这一步失败时它会把自己（meow）的原话打出来，
并且区分“配置不对”和“二进制在这台机器上跑不起来”，别一律当成配置问题去改。

`tools/pack.sh` 会检查 docker / plugin-tools / 宿主的 `plugin`，缺 plugin-tools 就在容器里
现编；然后逐平台 `SVCD_DOCKER=1 plugin-tools build --platform …`（`build.sh` 把编译丢进
`tools/docker` 的 `rust-cross` 镜像：clang+llvm、nasm、musl-tools、rustup 1.98.1、
cargo-xwin + 预下载的 MSVC CRT/SDK），最后把 `.spk` 拷到仓库的 `dist/`。
细节、挂载和国内加速见 `tools/docker/README.md`。

只想编一个二进制：

```sh
SVCD_DOCKER=1 sh build.sh meow 1.3.0 x86_64-pc-windows-msvc   # → out/meow.exe
sh tools/docker/run.sh -w plugins/meow -n meow -- cargo build --release --target <triple>
```

### 手工构建

```bash
plugin-tools build --platform x86_64-unknown-linux-gnu
plugin-tools check dist/meow-1.3.0-x86_64-unknown-linux-gnu.spk
```

Rust 1.98.1（vendor 里的 `rust-toolchain.toml`）和 cmake/C++ 是构建依赖；
BoringSSL 由 meow-rs 自己静态链接。构建只产出一个二进制（meow 内核），入口是
`payload/bin/` 里的两个脚本。

本机构建要满足三件事，缺哪件 `build.sh` 都会先说清楚再退出：

| 要的东西 | 说明 |
| --- | --- |
| 目标装到 pin 的工具链上 | `rustup target add --toolchain 1.98.1-<本机三元组> <目标>`。装到默认工具链不算数——`rust-toolchain.toml` 把版本固定在 1.98.1 |
| cmake | BoringSSL 用它构建；Windows 上 `scoop install cmake`，Linux 上 `apt install cmake` |
| 交叉 C 工具链 | musl 目标需要（`X86_64_MUSL_CROSS` / `AARCH64_CROSS` / `CROSS_PREFIX`）；`build.sh` 会探测 `/usr/soft/*-cross`、`/usr/local/x86_64-linux-musl-cross`、`/d/soft/x86_64-linux-musl`，找不到就报错。只找到 `musl-gcc` 时也能配起来，但 BoringSSL 一定链不上——装整份：`sh tools/cross/install.sh <triple> /usr/soft` |

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
