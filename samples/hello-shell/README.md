# hello-shell —— Shell 参考插件

和 `samples/hello`、`samples/hello-c`、`samples/hello-powershell` 能力一致。
POSIX sh 里没有 JSON 解析器，所以这里演示的是「用 SDK 的扁平扫描 + awk」能做到什么。

| 能力 | 演示什么 |
|---|---|
| `echo` | 文本入 / 文本出 |
| `get_stats` | JSON 文本出（调用数、uptime、pid） |
| `count_text` | `$blob` 输入：`awk` 流式统计行数与词数 |
| `get_file_info` | `$file` 文件包：`svcd_json_numbers` 取 part 列表，逐 part 读大小 |
| `create_report` | `$blobPath` 输出：写 `incoming/` 并设 `SVCD_BLOB_PATH` |
| 日志 | `echo … >&2` |
| 配置 | `svcd_config_get <key>` 读 `SVCD_CONFIG` |

## 跑起来

```bash
cd samples/hello-shell
plugin-tools build            # 无编译，直接打包（未签名）
plugin install dist/hello-shell-1.0.0-<triple>.spk
plugin start hello-shell
```

## 三个 shell 特有的约定

1. **日志走 stderr**：`echo … >&2`。stdout 是协议，写一行日志就断一条连接。
2. **大结果靠变量**：handler 里设置 `SVCD_BLOB_PATH` / `SVCD_BLOB_NAME` /
   `SVCD_BLOB_MIME`，SDK 会把回复换成 `{"$blobPath": …}`。handler 在**同一个 shell**
   里执行（stdout 重定向到临时文件），所以变量能传回来。
3. **能力名用首个匹配**：分发用 `svcd_field_first`，因为参数里可能也有
   `"name"`（文件名）；取字段用 `svcd_field`（最后一个匹配），参数是扁平结构时足够。

## 边界

- 没有 JSON 解析器：需要解析嵌套结构的插件请用 Rust / C / PowerShell。
- `interpreter.name` 写 `sh`，**不要**写 `version`——Debian 的 sh 是 dash，
  `--version` 探测会失败、安装被拒。

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
