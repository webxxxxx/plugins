# __NAME__ — Shell 插件模板

把一条已有命令包装成能力。只写 `case` 分支，协议在 `package/payload/lib/svcd-plugin.sh` 里。

## 布局

```
package/                        ← 打包根：里面有什么，包里就有什么
├── manifest.json                 ← schema 1；id 是 ASCII 身份（缺省=项目名），name 是显示名
└── payload/
    ├── bin/__NAME__.sh         ← 你的代码（提交）
    └── lib/svcd-plugin.sh      ← 协议壳（由 sdk/shell 同步，勿手改）
```

## 能力清单要和代码对上

manifest 的 `capabilities[]` 是 MCP 面上暴露的工具；`handle()` 里的 `case` 分支是实现。
两边一旦漂移，工具会"存在但调用必错"。shell 没法编译期检查，所以改一处就同步改另一处。

## 命令

```bash
plugin-tools template <name> --lang shell --webui   # 只创建项目
plugin-tools build                                   # 打包（默认本机三元组，未签名）
plugin-tools build --platform aarch64-unknown-linux-musl
plugin-tools check dist/xxx.spk
```

`--webui` 会带上 `package/payload/web/` 并在 manifest 里写 `ui`；它调用的就是
`uname` / `date` 两个能力，和后端是 shell 还是 rust 无关。

`pack` 的 `interpreter.name` 写的是 `sh`，且**故意不写 `version`**：`sh` 会走 `--version`
探测，而 Debian 的 sh 是 dash，探测会报非法选项、安装直接被拒。

Windows 上要求 `PATH` 里能找到 `sh`（Git Bash / MSYS）；否则请用 PowerShell 模板。

## 边界

POSIX sh 里没有 JSON 解析器，所以这个壳只给 handler 两样东西：能力名，和原样的
`arguments` 字符串。参数是扁平字符串时够用；需要真正解析 JSON 的插件请用 Rust 模板。
