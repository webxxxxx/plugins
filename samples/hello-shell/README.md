# hello-shell —— Shell 参考插件

和 `samples/hello`、`samples/hello-c`、`samples/hello-powershell` 能力一致。
POSIX sh 里没有 JSON 解析器，所以这里演示的是「用 SDK 的扁平扫描 + awk」能做到什么。

| 能力 | 演示什么 |
|---|---|
| `echo` | 文本入 / 文本出 |
| `stats` | JSON 文本出（调用数、uptime、pid） |
| `summarize` | `$blob` 输入：`awk` 流式统计行数与词数 |
| `inspect_file` | `$file` 文件包：`svcd_json_numbers` 取 part 列表，逐 part 读大小 |
| `report` | `$blobPath` 输出：写 `incoming/` 并设 `SVCD_BLOB_PATH` |
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
