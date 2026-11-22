# __NAME__ — C 插件模板

只写 handler：协议在 `src/svcd_plugin.h`（单头文件，无外部依赖）。

## 布局

```
package/                  ← 打包根，里面有什么包里就有什么
├── manifest.json       ← schema 1；id 是 ASCII 身份（缺省=项目名），name 是显示名
└── payload/{bin,web}/
src/main.c                ← 你的代码
src/svcd_plugin.h         ← 协议壳（由 sdk/c 同步，勿手改）
build.json                ← 构建配方
```

## 命令

```bash
plugin-tools template <name> --lang c --webui   # 只创建项目
plugin-tools build                               # gcc 编译 + 打包（默认未签名）
plugin-tools build --platform aarch64-unknown-linux-musl
```

构建用 `build.json` 里的 `gcc`。Windows 上需要 MinGW/MSYS 的 `gcc`；
没有就改用 `cl` 等编译器并同步改 `build.json`。交叉编译要换工具链：

```json
{ "build": { "command": ["aarch64-linux-gnu-gcc", "-static", "-o", "out/{name}", "src/main.c"],
             "artifact": "out/{name}" } }
```

## 边界

- `svcd_plugin.h` 是**扁平 JSON 扫描器**，不是通用解析器：`arguments` 以原样字符串
  交给 handler，取字段用 `svcd_string()`。需要真正解析 JSON 的插件请用 Rust 模板。
- stdout 是协议；日志用 `svcd_log()`（写 stderr）。
- 能力清单 `CAPABILITIES[]` 必须与 manifest 的 `capabilities[].name` 一致。
