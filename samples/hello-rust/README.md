# hello —— 参考插件

一个插件能做的事，这里都有：能力调用、配置、日志、后台任务、事件推送、**大对象输入输出**。
代码是生产级的：读配置有兜底、缺失参数返回错误而不是 panic、后台线程观察取消令牌、
大文件流式处理而不是整个读进内存、能力清单有测试兜底。

## 它演示了什么

| 能力 | 看哪里 |
|---|---|
| 配置读取（缺失即用默认） | `src/main.rs` 的 `Hello::new` |
| 结构化日志 → stderr | `log_info!` / `log_warn!` |
| 五种能力 + 错误处理 | `impl Plugin for Hello` |
| 后台任务 + 取消令牌 | `cancel.ticker(...)` |
| 推送事件给 UI | `svcd_plugin::emit` |
| **raw 流**（协议 3 的 stream ≥ 1） | `impl RawPlugin for Hello` + UI 的 raw 面板 |
| **blob 输入**（≤512 MiB，流式） | `count_text` + `blob_argument` |
| **file 包输入**（≤16 GiB，按 part 流式） | `get_file_info` + `file_argument` / `FileArg::open` |
| **blob 输出** | `create_report` + `BlobWriter` |
| 能力清单与 manifest 一致 | `declared_capabilities_match_the_manifest` |

## 跑起来

```bash
cd samples/hello-rust
plugin-tools build --platform <triple>   # 构建 + 打包（未签名）
plugin install dist/hello-rust-1.0.0-<triple>.spk
plugin start hello-rust
```

`build.json` 按 `cargo build --target <triple>` 构建，产物取 `target/<triple>/release/<name>`；
换平台前先 `rustup target add <triple>`，musl / ARM 还要对应的链接器。不给 `--platform`
时会按 `.plugin.json` 的三个平台逐个打，缺工具链的那几个会直接报错，不会拿主机产物顶替。

宿主需要 `allow_insecure_install = on`（未签名包）。打开 UI 后可以试 echo、上传一个
文本文件看摘要、生成报告并下载、看 tick 事件。

## 大对象：两级传输面

控制帧上限 1 MiB，所以大文件从不进 JSON。完整语义见
`plugin-manager/docs/plugin-blob.md`，这里是插件侧要点。

| 级别 | 上限 | 参数 | 读法 |
|---|---|---|---|
| blob | 512 MiB | `{"$blob": id}` → `{$blob, path, size}` | 直接按文件流式读 |
| file 包 | 16 GiB | `{"$file": id}` → `{$file, dir, manifest, size, name, mimeType}` | 读 manifest 的 parts，按 `part-<n>` 顺序流式读 |

**上传**（浏览器 → 插件）

```js
const meta = await ui.uploadBlob(file);                  // 整体流式，≤512 MiB
const big  = await ui.uploadFilePackage(file);           // part 会话，≤16 GiB
await ui.invoke('count_text',    { input: ui.blobArg(meta.id) });
await ui.invoke('get_file_info', { input: ui.fileArg(big.id) });
```

需要断点续传时给 `uploadId`：`uploadBlobResumable(file, { uploadId })` 或
`uploadFilePackage(file, { uploadId })`，服务端会告诉你已有哪些 part，只补缺的。
失败时错误对象上带 `uploadId`，下次接着传。

**输出**（插件 → 浏览器）

```rust
let mut writer = BlobWriter::create(&context, "report.txt", "text/plain")?;
writer.write_all(body.as_bytes())?;
writer.finish()          // → {"$blobPath": "…/incoming/…", "name": …, "mimeType": …}
```

宿主校验路径在 `incoming/` 内、算 SHA-256、落成 blob、删暂存文件，再把结果换成
`resource_link` + `structuredContent.$blob`。所以 UI 要用 `invokeResult` 才能拿到 id：

```js
const r = await ui.invokeResult('create_report');
await ui.downloadBlob(r.structuredContent.$blob, { filename: 'report.txt' });
```

**边界**：blob 会话与 file 会话不通用（拿错 id 会 409）；下载两个级别都支持
`Range` / `HEAD` / 条件请求；临时文件默认 2 小时 TTL。`$file` 不是 `$blob` 的别名，
它们是两个不同的层级。

## 协议 3：调用、推送与 raw 共用一条管道

`manifest.json` 写 `"protocol": 3`，`main` 用 `multiplex::serve`。帧是
`u32 len | u32 stream_id | payload`：

- **stream 0** 上跑的还是那套 JSON-RPC：五种能力、`notifications/*`、宿主发来的 ping。
- **stream ≥ 1** 是不透明字节，宿主按 stream 配对给 `/raw` 的 WebSocket 订阅者。
  插件只知道「帧是从哪条 stream 来的」，回帧就用同一条。

写侧有界、不阻塞读循环：`Streams::send(stream, bytes)` 满了返回 `SendError::Busy`，
超限的帧在写头之前就被拒。本样例每收到一帧 raw，就在同一条 stream 上回一帧，并推一条
`{"type":"raw"}` 通知（UI 的 raw 面板能同时看到两者）。

**关流走控制流**：stream `0xFFFFFFFF` 带 `{"op":"close","stream":N}`。页面的 raw WebSocket
断开时，宿主给插件发 close → `raw_closed` 被调用（样例推一条 `{"type":"raw","closed":true}`
通知，SSE 上能看到）。插件也能用 `Streams::close(stream)` 主动结束一条流，宿主会把该
WebSocket 干净关闭（1000）。往已经被关掉的流写数据，宿主会回一个 close 而不是静默丢弃。

## 它演示了宿主哪些能力

| 宿主能力 | 在这个样例里怎么触发 |
|---|---|
| 方法调用 | `echo`（文本往返）、`get_stats`（JSON 文本） |
| 文件上传（≤512 MiB，`$blob`） | `count_text`：UI 上传 → 宿主 blob store → 插件按文件流式读 |
| 大文件上传（≤16 GiB，`$file` 文件包） | `get_file_info`：part 会话 → manifest → 逐 part 读 |
| 文件下载（`$blobPath`） | `create_report`：写 `SVCD_TEMP_DIR/incoming/` → 宿主落 blob → UI GET |
| SSE | 每次能力调用推一条 `notifications/message`，UI 的 `./events` 上能看到 |
| WebSocket | 同一批通知；`./ws` 上还可以直接发 `tools/call` |
| raw 流（协议 3） | UI 的 raw 面板 → `/raw` WebSocket → stream ≥ 1 的二进制往返 |
| 日志 | 每次调用写一行 stderr，宿主落到 `<logs>/<id>.log` 并轮转 |

也就是说：**能力（capabilities）只有一种形状**（`tools/call`），文件上传/下载是它携带
`$blob` / `$blobPath` 的两个特例，SSE/WS 是同一份通知的两条承载，日志走 stderr。
