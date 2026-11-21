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

宿主的 `lifecycle.start` 就是被监督的前台进程，所以 `sshd.sh start` 会
`exec` 到 `dropbear -F`，而不是拉起后台再退出。`stop` 按 pidfile 结束进程，
`status` 输出一行 JSON 供 `svc status` 的 probe 显示。

`exec.ready` 写 `exec`：这个插件没有协议通道，宿主启动后立即视为 ready。
`exec.path` 仍指向同一个脚本，只是被 `lifecycle.start` 覆盖，不会单独执行。

## 配置

`plugin editconfig sshd` 编辑：

```ini
port = 2222
```

默认 2222，因为 user mode 下不能绑定 22。root 安装时改成 22 即可。
登录认证沿用系统账户：密码或目标用户的 `~/.ssh/authorized_keys`。

## SFTP

dropbear 的 sftp 子系统路径优先读环境变量 `SVCD_SFTP_SERVER`；
`sshd.sh start` 把它设成包内的 `payload/bin/sftp-server` 后再 `exec` dropbear。
这样不写死安装路径、不要求 root 写 `/usr/libexec`，user mode 下也能用 SFTP。
环境变量缺失时才回退到 dropbear 编译时的默认路径。

## 构建

```bash
plugin-tools build --platform x86_64-unknown-linux-gnu
plugin-tools check dist/sshd-1.0.0-x86_64-unknown-linux-gnu.spk
```

平台只声明 Linux；dropbear 没有 Windows 移植，macOS 下静态链接也不成立。
