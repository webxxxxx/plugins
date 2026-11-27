# sshfs — SSHFS 挂载

把旧固件里 `autostart/05-nas` + `sh/mount-sshfs.sh` + `sh/umount-sshfs.sh` 合成一个
svcd 插件（插件 id 从 `nas` 改为 `sshfs`）：sshfs、dropbear 的 `dbclient`、`fusermount3`、
`dropbearkey` 随包走，挂载/卸载交给宿主的生命周期。

支持 Linux 的四个三元组（`x86_64` / `aarch64` / `armv7` 的 musl，以及 `x86_64` gnu），
没有 `compat` 限制：包里没有内核模块，任何 Linux 都能装。四个二进制**打包前从 vendor
里的源码编译**（pcre2 / libffi / zlib / glib / libfuse3 / sshfs / dropbear 的
dbclient+dropbearkey），配方来自 `sr6110_rebuild` 的 `thirdparty/sshfs/build.sh` 与
`thirdparty/dropbear/build.sh`。

上游 sshfs 本身还支持 macOS（要 macFUSE 的内核扩展和另一套 mount helper）和 BSD；
这两个都不在“插件自带全部依赖”的范围里：Windows 没有 FUSE，macOS 的挂载依赖系统扩展，
所以只出 Linux 包。

## 布局

```
build.json                构建配方（artifact: out/sshfs）
build.sh                  从 vendor/ 源码编依赖链和 sshfs/dbclient/fusermount3/dropbearkey
package/
├── manifest.json         平台无关字段 + compat.devices
├── config/config         用户可编辑的配置（连接参数没有默认值，见下）
└── payload/bin/
    ├── sshfs.sh          生命周期：start / stop / status
    ├── sshfs             构建产物（源码见 vendor/）
    ├── dbclient          构建产物（dropbear 源码见 vendor/）
    ├── fusermount3       构建产物（libfuse3 源码见 vendor/）
    └── dropbearkey       构建产物：首次启动生成 SSH 密钥
vendor/*.tar.*            上游源码 tarball（见 vendor/README.md）
```

## 生命周期

旧版 `start()` 跑完 `mount-sshfs.sh` 就结束，挂载靠 `exec sshfs` 留在前台。标准插件
里被监督的进程必须是常驻进程，正好就是 sshfs：

| 动作 | 做什么 |
|---|---|
| `exec`（`start`） | 首次启动生成一套 SSH 密钥、检查挂载点、清掉残留挂载，`exec sshfs`（前台）。连接断了/进程被终止，挂载随之失效，宿主看到进程退出并重启。 |
| `lifecycle.stop` | 进程消失**之后**收尾：`fusermount3 -u` → `umount` → `umount -l`。宿主强杀（SIGKILL）后挂载点仍然在，这一步就是为它准备的。 |
| `lifecycle.status` | 一行 JSON（`mounted` / `mountpoint` / `host`），给 `svc status` 的 probe 用。 |

`exec.ready=exec`：插件没有协议通道，spawn 成功即视为 ready。

旧版还会在挂载成功后把「装在 NAS 上的插件」拉起来。标准插件不复制这条隐藏逻辑：
需要它的插件在清单里写 `dependencies: ["sshfs"]`，由宿主的依赖图保证先起 sshfs、
停的时候反过来先停依赖它的插件。

## 标准位置

| 内容 | 位置 | 说明 |
|---|---|---|
| 配置 | `<插件目录>/config` | 宿主管：安装时复制模板、`plugin editconfig` 编辑、升级保留 |
| SSH 私钥/公钥 | `SSHFS_KEY` | 未填时默认 `<插件目录>/data/ssh/id_ed25519`；不存在时首次启动生成一套 |
| 日志 | stderr | 宿主写进自己的日志目录（Linux 默认 `/var/log/svcd/sshfs.log`）并轮转；插件不自建日志文件 |
| 挂载点 | `SSHFS_MNT` | 未填时默认 `<插件目录>/data/mnt`；也可以填绝对路径挂到别处 |

不填时都落在插件目录内；填了绝对路径就按用户的来（相对路径仍按插件目录解释）。
插件二进制只从 `payload/bin/` 取，系统命令走 PATH。

## 配置

`plugin editconfig sshfs` 编辑。这四项**没有默认值**，必须填：

| 项 | 必填 | 说明 |
|---|---|---|
| `SSHFS_HOST` | 是 | 远端地址 |
| `SSHFS_USER` | 是 | 登录用户 |
| `SSHFS_PATH` | 是 | 要挂载的远端目录 |
| `SSHFS_PORT` | 是 | SSH 端口 |
| `SSHFS_MNT` | 否 | 默认 `<插件目录>/data/mnt`；也可写绝对路径 |
| `SSHFS_KEY` | 否 | 默认 `<插件目录>/data/ssh/id_ed25519`；也可写绝对路径 |
| `SSHFS_ALIVE` | 否 | 默认 `15`（ServerAliveInterval 秒） |

密钥：如果 `SSHFS_KEY` 指向的文件不存在，首次启动用包里的 `dropbearkey` 生成一套
ed25519 密钥（私钥 600，公钥写到 `<SSHFS_KEY>.pub`）。把公钥加到远端用户的
`~/.ssh/authorized_keys` 才能登录；日志里会打印公钥路径。

## 产物

```
sshfs-1.2.0-x86_64-unknown-linux-musl.spk
sshfs-1.2.0-aarch64-unknown-linux-musl.spk
sshfs-1.2.0-armv7-unknown-linux-musleabihf.spk
sshfs-1.2.0-x86_64-unknown-linux-gnu.spk
```

musl 三个是全静态；gnu 那个动态链接 glibc（静态 glibc 的 DNS 解析要 dlopen
`libnss_dns.so`，自包含包里拿不到），所以 gnu 包在 CI 的 ubuntu-22.04 上编，基线是
glibc 2.35。

## 安装

通用包，直接装、填必填配置、启动：

```sh
plugin install sshfs-1.2.0-aarch64-unknown-linux-musl.spk
plugin editconfig sshfs      # 至少填 SSHFS_HOST / SSHFS_USER / SSHFS_PATH / SSHFS_PORT
plugin start sshfs
```

**和旧服务不要同时开**：旧固件 `svc nas start` 挂的是 `/var/nas`，和插件不是同一个
挂载点。切换时先 `svc nas stop && svc nas disable`，再启动插件。

## 构建

和 `meow` / `sshd` / `wireguard` 一样，**打包前从 `vendor/` 源码编译**，没有预编译件。

```bash
plugin-tools build --platform aarch64-unknown-linux-musl
plugin verify dist/sshfs-1.1.0-aarch64-unknown-linux-musl-sr6110.spk
```

交叉工具链走 `tools/cross-env.sh`（`aarch64-linux-musl-gcc`），宿主另外需要
`meson` / `ninja` / `pkg-config`（`apt install meson ninja-build pkg-config`，或者
`pip install meson`）。依赖链每次都从干净的 `out/build/` 开始重编，一次约几分钟；
`vendor/` 缺 tarball、宿主缺工具、或目标工具链找不到时构建直接失败。

## 已知取舍

- 挂载点不填时默认落在插件目录内（`data/mnt`）；要挂到 `/var/nas` 这类系统路径，
  在配置里写绝对路径即可。旧固件 `/var/nas/plugins` 那套「装在 NAS 上的插件」不再
  适用：需要它的插件写 `dependencies: ["sshfs"]`，由宿主正常安装和排序。
- `sshfs` 断了以后宿主会按退避重启；远端一直离线时会进入 failed，恢复网络后需
  `plugin start sshfs`。
