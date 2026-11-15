# hello-powershell —— PowerShell 参考插件

和 `samples/hello`、`samples/hello-c`、`samples/hello-shell` 能力一致。

| 能力 | 演示什么 |
|---|---|
| `echo` | 文本入 / 文本出（前缀取自配置里的 `greeting`） |
| `stats` | JSON 文本出（调用数、uptime、pid） |
| `summarize` | `$blob` 输入：按行流式读宿主给的文件 |
| `inspect_file` | `$file` 文件包：`Get-SvcdFileParts` 按 manifest 顺序读 part |
| `report` | `$blobPath` 输出：`New-SvcdIncomingPath` + 返回 `@{BlobPath=…}` |
| 日志 | `Write-SvcdLog` → stderr |
| 配置 | `Get-SvcdConfig` 读 `SVCD_CONFIG` |

## 跑起来

```bash
cd samples/hello-powershell
plugin-tools build            # 无编译，直接打包（未签名）
plugin install dist/hello-powershell-1.0.0-<triple>.spk
plugin start hello-powershell
```

## 两个平台差异

| 目标 | `interpreter.name` |
|---|---|
| Windows | `powershell`（系统自带 5.1，实测通过） |
| Linux / macOS | `pwsh`（PowerShell 7） |

模板默认写 `powershell`。跨到 Linux 时改 manifest 并把 `-File` 参数保留即可。

## 边界

- stdin/stdout 是协议：日志走 `Write-SvcdLog`，**不要** `Write-Output`。
- `interpreter.version` 留空：`powershell --version` 不是合法参数，写了会导致安装被拒。
- 能力清单 `$Capabilities` 必须与 manifest 的 `capabilities[].name` 一致。
