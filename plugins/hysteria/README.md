# hysteria — Hysteria2 服务端

把 [mini-sb-agent](https://github.com/ashvvvvv/mini-sb-agent)（sing-box 内核）
编译成 svcd 插件，用一份 sing-box 配置跑 Hysteria2 入站。插件没有 JSON-RPC
能力；生命周期是 `start` / `stop` / `status`。

## 布局

```
vendor/mini-sb-agent/      Go 源码
certgen/                   自签证书 / 随机密码生成器（只用标准库）
build.json                 go build 配方
build.sh                   平台映射，编内核也编 certgen
package/
├── manifest.json
├── config/config          sing-box 配置模板（@CERT@ / @KEY@ / @PASSWORD@ 占位）
└── payload/bin/hysteria.sh 生命周期包装
    ├── mini-sb-agent       构建产物
    └── hysteria-certgen    构建产物
```

## 运行方式

宿主把 `lifecycle.start` 当成常驻前台进程，所以 `hysteria.sh start` 直接
`exec mini-sb-agent -config …`。启动前会：

1. 从活动配置（`plugin editconfig hysteria`）或 `package/config/config` 取模板；
2. 首次启动用包里自带的 `hysteria-certgen` 生成自签 TLS 证书到
   `<插件目录>/data/`（即 `$SVCD_PLUGIN_DIR/data/`）；
3. 首次启动生成 32 位十六进制密码到同目录的 `password`；
4. 替换 `@CERT@`、`@KEY@`、`@PASSWORD@`，再启动内核。

证书和密码不依赖机器的 `openssl`（很多设备上没有）：`certgen/` 只用 Go
标准库，交叉编译不需要 cgo，跟着包一起发。直接从源码手跑、包里还没有它时，
才退回 `openssl`（密码再退回 `od`）。

默认入站 `0.0.0.0:8443/udp`（Hysteria2 基于 QUIC），出站 `direct`。要接
meow 的本地代理，把配置的 `route.final` 改成 `meow`，并加一个
`{"type":"socks","tag":"meow","server":"127.0.0.1","server_port":7890}`
出站。

客户端连接信息：

```bash
cat "$SVCD_PLUGIN_DIR/data/password"   # 默认 /data/svcd/data/plugins/hysteria/data/password
```

自签证书需要在客户端跳过证书校验，或把 `cert.pem` 分发给客户端。

### 配置里的占位符

控制台编辑的是活动配置，里面会出现三个占位符：

| 占位符 | 启动时替换成 |
| --- | --- |
| `@PASSWORD@` | `<插件目录>/data/password` 的内容（首次启动生成） |
| `@CERT@` | `<插件目录>/data/cert.pem` |
| `@KEY@` | `<插件目录>/data/key.pem` |

- 保存后需要 `plugin restart hysteria`（或 stop/start）才生效；脚本每次启动都会重新渲染。
- 想改密码：改 `password` 文件后重启，或者把 `@PASSWORD@` 直接替换成新的字面密码。
- 想用自己的证书：把 `@CERT@` / `@KEY@` 替换成证书和私钥的实际路径；替换后不再自动生成自签证书。
- 其他字段（`listen_port`、`outbounds`、`route` 等）直接改，重启即生效。

## 构建

```bash
plugin-tools build --platform x86_64-unknown-linux-gnu
plugin-tools check dist/hysteria-1.0.0-x86_64-unknown-linux-gnu.spk
```

需要 Go 1.25+（vendor 的 `go.mod`）和网络拉模块依赖；`certgen/` 只用标准库，
不需要联网。平台声明为 Linux + macOS；Windows 不在列表里，因为入口是 POSIX sh 包装。
