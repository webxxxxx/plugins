# hello-c —— C 参考插件

和 `samples/hello-rust` 能力一致，用来对照两套 SDK 的写法。

| 能力 | 演示什么 |
|---|---|
| `echo` | 文本入 / 文本出 |
| `get_stats` | JSON 文本出（调用数、uptime、pid） |
| `count_text` | `$blob` 输入：打开宿主给的文件路径，流式统计 |
| `get_file_info` | `$file` 文件包：读 manifest 的 part 列表，逐 part 读大小 |
| `create_report` | `$blobPath` 输出：写 `SVCD_TEMP_DIR/incoming/` 并返回 |
| 日志 | `svcd_log()` → stderr |

## 跑起来

```bash
cd samples/hello-c
plugin-tools build                 # gcc 编译 + 打包（未签名）
plugin install dist/hello-c-1.0.0-<triple>.spk
plugin start hello-c
```

交叉编译不用改 `build.json`：命令里的 `{cc}` 取 `$CC`，没设才退回 `gcc`。

```bash
CC=/usr/soft/x86_64-linux-musl/bin/x86_64-linux-musl-gcc \
  plugin-tools build --platform x86_64-unknown-linux-musl
```

musl 目标可以让 `tools/cross/install.sh <triple>` 把工具链和 `CC` 一起准备好
（CI 里它同时写 `$GITHUB_ENV` / `$GITHUB_PATH`）。

## 与目标机相关的两个点

- **`interpreter`**：无。C 编译成原生二进制，直接 `exec`。
- **`$file` 的 part 名**：宿主写成 `part-00001`（5 位补零），本示例按 manifest 的
  `partNumber` 拼路径，不依赖目录顺序。

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
