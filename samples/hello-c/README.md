# hello-c —— C 参考插件

和 `samples/hello` 能力一致，用来对照两套 SDK 的写法。

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

交叉编译要换工具链，改 `build.json`：

```json
{ "build": { "command": ["aarch64-linux-gnu-gcc", "-static", "-o", "out/{name}", "src/main.c"],
             "artifact": "out/{name}" } }
```

## 与目标机相关的两个点

- **`interpreter`**：无。C 编译成原生二进制，直接 `exec`。
- **`$file` 的 part 名**：宿主写成 `part-00001`（5 位补零），本示例按 manifest 的
  `partNumber` 拼路径，不依赖目录顺序。
