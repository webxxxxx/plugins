# hello —— 参考插件

一个插件能做的事，这里都有：能力调用、配置、日志、后台任务、事件推送、分块上传下载。
代码是生产级的：读配置有兜底、缺失参数返回错误而不是 panic、后台线程观察取消令牌、
能力清单有测试兜底。

## 它演示了什么

| 能力 | 看哪里 |
|---|---|
| 配置读取（缺失即用默认） | `src/main.rs` 的 `Hello::new` |
| 结构化日志 → stderr | `log_info!` / `log_warn!` |
| 五种能力 + 错误处理 | `impl Plugin for Hello` |
| 后台任务 + 取消令牌 | `cancel.ticker(...)` |
| 推送事件给 UI | `svcd_plugin::emit` |
| 分块上传下载 | `FileStore`（`file_put` / `file_get` / `file_list`） |
| 能力清单与 manifest 一致 | `declared_capabilities_match_the_manifest` |

## 跑起来

```bash
cd samples/hello
node ../../tools/plugin-build.mjs      # 构建 + 打包（未签名）
plugin install dist/hello-1.0.0-<triple>.spk
plugin start hello
```

宿主需要 `allow_insecure_install = on`（未签名包）。打开 UI 后可以试 echo、
上传一个文件再下载回来、看 tick 事件。

## 文件传输协议

宿主只给插件一条请求通道，一帧上限 1 MiB，所以文件走 base64 分块：

```
file_put  {path, offset, total, data(base64), final}
          → {"ok":true,"path":…,"size":…,"complete":true}
file_get  {path, offset, length}
          → {"data":…, "size":…, "eof":…}
```

浏览器半边在 `sdk/web` 的 `uploadFile` / `downloadFile`，服务端半边在
`sdk/rust` 的 `FileStore`——两边一起改，协议才对得上。文件名只允许单层
`[A-Za-z0-9._-]`，避免越出存储目录。
