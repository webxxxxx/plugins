# hello —— 参考插件

一个插件能做的事，这里都有：能力调用、配置、日志、后台任务、事件推送、**大对象输入输出**。
代码是生产级的：读配置有兜底、缺失参数返回错误而不是 panic、后台线程观察取消令牌、
大文件流式处理而不是整个读进内存、能力清单有测试兜底。

## 它演示了什么

| 能力 | 看哪里 |
|---|---|
| 配置读取（缺失即用默认） | `src/main.rs` 的 `Hello::new` |
| 结构化日志 → stderr | `log_info!` / `log_warn!` |
| 四种能力 + 错误处理 | `impl Plugin for Hello` |
| 后台任务 + 取消令牌 | `cancel.ticker(...)` |
| 推送事件给 UI | `svcd_plugin::emit` |
| **blob 输入**（流式） | `summarize` + `blob_argument` |
| **blob 输出** | `report` + `BlobWriter` |
| 能力清单与 manifest 一致 | `declared_capabilities_match_the_manifest` |

## 跑起来

```bash
cd samples/hello
node ../../tools/plugin-build.mjs      # 构建 + 打包（未签名）
plugin install dist/hello-1.0.0-<triple>.spk
plugin start hello
```

宿主需要 `allow_insecure_install = on`（未签名包）。打开 UI 后可以试 echo、上传一个
文本文件看摘要、生成报告并下载、看 tick 事件。

## 大对象：`$blob` 与 `$blobPath`

控制帧上限 1 MiB，所以大文件从不进 JSON。完整语义见
`plugin-manager/docs/plugin-blob.md`，这里是插件侧要点：

**输入**（浏览器 → 插件）

```js
const meta = await ui.uploadBlob(file);          // POST /api/v1/blobs → {id, size}
await ui.invoke('summarize', { input: ui.blobArg(meta.id) });
```

宿主把 `{"$blob": id}` 替换成 `{"$blob": id, "path": "/…", "size": n}` 再交给插件。
插件拿到的是**只读文件路径**——按流读，别整个 `read_to_string` 一个 512 MiB 的文件。

**输出**（插件 → 浏览器）

```rust
let mut writer = BlobWriter::create(&context, "report.txt", "text/plain")?;
writer.write_all(body.as_bytes())?;
writer.finish()          // → {"$blobPath": "…/incoming/…", "name": …, "mimeType": …}
```

宿主校验路径在 `incoming/` 内、算 SHA-256、落成 blob、删暂存文件，再把结果换成
`resource_link` + `structuredContent.$blob`。所以 UI 要用 `invokeResult` 才能拿到 id：

```js
const r = await ui.invokeResult('report');
await ui.downloadBlob(r.structuredContent.$blob, { filename: 'report.txt' });
```

**边界**：单 blob ≤ 512 MiB；上传必须带 `Content-Length`，不支持 chunked / 断点续传；
`GET /api/v1/blobs/<id>` 不支持 Range；临时文件默认 2 小时 TTL。
