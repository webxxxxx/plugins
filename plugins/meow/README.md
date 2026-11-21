# meow — mihomo 兼容代理内核

把 [meow-rs](https://github.com/madeye/meow-rs) 编译成 svcd 插件。插件没有
JSON-RPC 能力；生命周期是 `start` / `stop` / `status`，启动后由 meow 自己
提供 mixed 代理端口和 YACD 面板。

## 布局

```
vendor/meow-rs/            源码
build.json                 cargo 构建配方
build.sh                   平台映射与产物复制
package/
├── manifest.json
├── config/config          用户可编辑的 YAML（安装后复制到 plugins/meow/config）
└── payload/
    ├── bin/meow.sh        生命周期包装
    ├── bin/meow           构建产物
    ├── web/index.html     控制台「打开」入口（跳转到 9090/ui/）
    ├── cn_domains.yaml    rule-provider
    └── yacd/              面板静态文件
```

## 运行方式

- `mixed-port: 7890`：HTTP/SOCKS5 混合代理。
- `external-controller: 127.0.0.1:9090`，YACD 在 `http://127.0.0.1:9090/ui/`。
- svcd 控制台的「打开」入口由 `manifest.ui` + `payload/web/index.html` 提供；
  页面会自动跳到 `http://<当前主机>:9090/ui/`。
- 默认只做 `DIRECT` 直出；在配置里加 `proxies` 并把 `rules` 的 `MATCH`
  改到代理组后即可用面板切换。

宿主的 `lifecycle.start` 是前台进程，所以 `meow.sh start` 直接
`exec meow`；启动前先跑 `meow -t` 校验配置，失败就返回非零而不是秒退。
`stop` 按 pidfile 结束进程，`status` 输出 JSON。

## 配置

```bash
plugin editconfig meow
```

配置直接使用安装后的活动配置 `plugins/meow/config`：

- 首次启动把模板里的 `@DATA@` 渲染成数据目录，之后不再覆盖；
- YACD 的 `/api/subscriptions` 写回的就是这个文件，所以在控制台里能看到，
  重启也不会丢；
- 从旧布局升级时，如果数据目录的 `config.yaml` 里已有 `subscriptions:`，
  第一次启动会把它迁回活动配置；
- 只有源码 checkout 没有活动配置时，才用 `package/config/config` 生成
  `$SVCD_DATA_DIR/meow/config.yaml` 作为副本；
- 数据目录缺失时回退到插件目录下的相对路径 `.data/meow`。

## 构建

```bash
plugin-tools build --platform x86_64-unknown-linux-gnu
plugin-tools check dist/meow-1.0.0-x86_64-unknown-linux-gnu.spk
```

Rust 1.98.1（vendor 里的 `rust-toolchain.toml`）和 cmake/C++ 是构建依赖；
BoringSSL 由 meow-rs 自己静态链接。Windows 不在平台列表里：插件入口是
POSIX sh 包装。

`.plugin.json` 的 `ci.platforms` 是发布流水线实际构建的子集：Linux x86_64
gnu + macOS 两种架构。Linux musl / armv7 需要额外的 BoringSSL 交叉链，
本地可以用 `plugin-tools build --platform ...` 自行尝试。
