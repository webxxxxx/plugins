# __NAME__ — PowerShell 插件模板

只写能力函数：协议在 `package/payload/lib/SvcdPlugin.ps1`（由 `sdk/powershell` 同步）。

## 布局

```
package/
├── manifest.json          ← interpreter 写 powershell，不写 version
└── payload/{bin,lib,web}/
```

## 命令

```bash
plugin-template <name> --lang powershell --webui
plugin-build                # 无编译，直接打包（默认未签名）
```

## 两个平台差异

| 目标 | `interpreter.name` |
|---|---|
| Windows | `powershell`（系统自带 5.1） |
| Linux / macOS | `pwsh`（PowerShell 7） |

模板默认写 `powershell`。跨到 Linux 时把 manifest 改成 `pwsh`，并确认目标机装了。

## 边界

- stdin/stdout 是协议：日志用 `Write-SvcdLog`（写 stderr），不要 `Write-Output`。
- 能力清单 `$Capabilities` 必须与 manifest 的 `capabilities[].name` 一致。
- `interpreter.version` 留空：`powershell --version` 并不是合法参数，写了会被探测失败。
