# hello-powershell —— PowerShell 参考插件

和 `samples/hello`、`samples/hello-c`、`samples/hello-shell` 能力一致。

| 能力 | 演示什么 |
|---|---|
| `echo` | 文本入 / 文本出（前缀取自配置里的 `greeting`） |
| `get_stats` | JSON 文本出（调用数、uptime、pid） |
| `count_text` | `$blob` 输入：按行流式读宿主给的文件 |
| `get_file_info` | `$file` 文件包：`Get-SvcdFileParts` 按 manifest 顺序读 part |
| `create_report` | `$blobPath` 输出：`New-SvcdIncomingPath` + 返回 `@{BlobPath=…}` |
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

## 它演示了宿主哪些能力

| 宿主能力 | 在这个样例里怎么触发 |
|---|---|
| 方法调用 | `echo`（文本往返）、`get_stats`（JSON 文本） |
| 文件上传（≤512 MiB，`$blob`） | `count_text`：UI 上传 → 宿主 blob store → 插件按文件流式读 |
| 大文件上传（≤16 GiB，`$file` 文件包） | `get_file_info`：part 会话 → manifest → 逐 part 读 |
| 文件下载（`$blobPath`） | `create_report`：写 `SVCD_TEMP_DIR/incoming/` → 宿主落 blob → UI GET |
| SSE | 每次能力调用推一条 `notifications/message`，UI 的 `./events` 上能看到 |
| WebSocket | 同一批通知；`./ws` 上还可以直接发 `tools/call` |
| 日志 | 每次调用写一行 stderr，宿主落到 `<logs>/<id>.log` 并轮转 |

也就是说：**能力（capabilities）只有一种形状**（`tools/call`），文件上传/下载是它携带
`$blob` / `$blobPath` 的两个特例，SSE/WS 是同一份通知的两条承载，日志走 stderr。
