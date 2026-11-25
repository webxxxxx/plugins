# sshd — dropbear + sftp-server

把 dropbear（多合一 SSH 服务端）和 OpenSSH 的 `sftp-server` 打包成 svcd 插件。
插件没有 JSON-RPC 能力；它只有生命周期：`start` / `stop` / `status`。

## 布局

```
build.json                构建配方：编译 dropbear 与 sftp-server
package/
├── manifest.json
├── config/config         用户可编辑的配置（port）
└── payload/
    ├── bin/sshd.sh       生命周期包装
    ├── bin/sshd          构建产物：dropbearmulti
    └── bin/sftp-server   构建产物：OpenSSH sftp-server
vendor/                   dropbear / openssh 源码
```

## 运行方式

宿主的 `exec` 就是被监督的进程：`exec.args` 是 `start`，所以 `sshd.sh start` 在前台
准备好运行目录、主机密钥和 SFTP 软链，然后 `exec` 到 `dropbear -F`——宿主看到的始终是
dropbear 本身（信号与进程组都归它）。

`stop` 是**收尾**：宿主先终止进程（Unix 进程组 SIGTERM → 10 秒 → SIGKILL），`stop`
之后才跑，删掉强杀留下的 pidfile；端口还听着只报一声，不发信号——强杀或断电留下的
pid 可能已经被系统复用给别的进程，`status` 也不会把它当成自己。它不负责杀进程。
`status` 输出一行 JSON 供 `svc status` 的 probe 显示。

`exec.ready` 写 `exec`：这个插件没有协议通道，宿主启动后立即视为 ready。

## 配置

`plugin editconfig sshd` 编辑：

```ini
port = 2222
```

默认 2222，因为 user mode 下不能绑定 22。root 安装时改成 22 即可。
登录认证沿用系统账户：密码或目标用户的 `~/.ssh/authorized_keys`。

## SFTP

两条路，指向同一个二进制：

- `sshd.sh start` 把 `SVCD_SFTP_SERVER` 设成包内的 `payload/bin/sftp-server`，dropbear
  优先读这个环境变量；
- 同时把 `/etc/sshd/sftp-server` 这条软链指过去——它就是 dropbear 编译时的默认路径
  （`build.sh` 用 dropbear 的 `localoptions.h` 把 `SFTPSERVER_PATH` 钉在这个值上）。
  选 `/etc` 下是因为有的路由器只有 `/etc` 可写，`/usr/libexec` 根本不存在。

`/etc` 不可写（只读根、非 root 安装）时，建软链只告警、start 不失败，环境变量那条路
还在。`sftp-server` 是以登录用户身份执行的，所以 `sshd.sh` 顺手把它的权限放开到 0755，
免得安装包里只剩 root 能读。

## 构建

```bash
plugin-tools build --platform x86_64-unknown-linux-gnu
plugin-tools check dist/sshd-1.0.0-x86_64-unknown-linux-gnu.spk
```

平台只声明 Linux；dropbear 没有 Windows 移植，macOS 下静态链接也不成立。
