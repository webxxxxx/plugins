# __NAME__ — PowerShell 插件模板

只写能力函数：协议在 `package/payload/lib/SvcdPlugin.ps1`（由 `sdk/powershell` 同步）。

## 布局

```
package/
├── manifest.json          ← schema 1；id 是 ASCII 身份、name 是显示名；interpreter 由工具写
└── payload/{bin,lib,web}/
```

## 命令

```bash
plugin-tools template <name> --lang powershell --webui
plugin-tools build                # 无编译，直接打包（默认未签名）
```

## 两个平台差异

| 目标 | `interpreter.name` |
|---|---|
| Windows | `powershell`（系统自带 5.1） |
| Linux / macOS | `pwsh`（PowerShell 7） |

`plugin-tools template` 会根据 `--platforms` 自动写：Windows 用 `powershell`，
Linux / macOS 用 `pwsh`。同一项目不允许同时选 Windows 和非 Windows 平台，
因为一个 manifest 只能有一个解释器名；需要两边就 scaffold 两个项目。
目标机必须确实装了对应的解释器。

## 边界

- stdin/stdout 是协议：日志用 `Write-SvcdLog`（写 stderr），不要 `Write-Output`。
- 能力清单 `$Capabilities` 必须与 manifest 的 `capabilities[].name` 一致。
- `interpreter.version` 留空：`powershell --version` 并不是合法参数，写了会被探测失败。
