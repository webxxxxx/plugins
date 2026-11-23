# 插件仓库

一个承载插件源码、语言模板与发布产物的单一仓库：源码与产物同源，产物只由流水线产生，从不手写。

## Language

**插件仓库**:
存放插件源码、模板与发布产物的单一仓库；任何进入 catalog 的包都必须由这里的流水线构建。
_Avoid_: 市场、应用商店、插件中心

**模板（Template）**:
`templates/<lang>/` 下一个可被 CI 直接构建的最小插件骨架；它的可用性由构建证明，不由文档声明。
_Avoid_: 脚手架、样例（样例是 `plugins/` 下的真实插件）

**源清单（Source manifest）**:
插件目录里由人写的 `manifest.json`，不含 `files[]`；`files[]` 只能由打包器计算。
_Avoid_: 插件配置、plugin.json

**插件 id（Plugin id）**:
源清单里的 `id`，插件的唯一身份：ASCII、`[a-z0-9][a-z0-9_-]{0,63}`，不能是 `svc`；源清单不写时
打包器取项目目录名。安装目录、日志文件名、MCP 工具名前缀（`<id>.<能力>`）、`/plugin-ui/<id>/`、
产物文件名都用它。
_Avoid_: 插件名、包名（那是显示名或文件名）

**显示名（Display name）**:
源清单里的 `name`，只给人看：任意 UTF-8，可中文，缺省等于 `id`。命令与 API 只认 `id`，
不按显示名解析。
_Avoid_: 插件名（有歧义）

**输出契约（outputSchema）**:
能力上可选的 `outputSchema`，声明该能力结果的结构；只投影进 MCP `tools/list`，运行期不校验
（不写 = 未声明，`{}` = 任意）。
_Avoid_: 返回值校验、结果 schema

**包图标（icons）**:
源清单里可选的 `icons: {png?, ico?, icns?}`，值必须是包内文件；构建校验存在与非空，验包再校验
魔数与大小，不做转格式或转尺寸。
_Avoid_: 图标资源、logo

**协议 3（Protocol 3）**:
宿主与插件之间的一种传输：`u32 payload_len | u32 stream_id | payload` 帧；stream 0 承载 JSON-RPC
（能力调用、通知、ping），stream ≥ 1 是不透明字节，配给 `/raw` 的 WebSocket 订阅者。一条管道
同时跑调用、推送与 raw，替代"要么 JSON 要么 raw"的互斥模式。
_Avoid_: 二进制协议、raw 协议（协议 3 不止 raw）

**stream 0**:
协议 3 里承载 JSON-RPC 的那条流，语义与协议 1 的换行 JSON 相同；单帧上限 1 MiB（宿主 `CALL_FRAME_MAX`）。
_Avoid_: 控制流、主通道

**raw 流（Raw stream）**:
协议 3 里 stream ≥ 1 的通道：插件按 stream 写不透明字节，宿主转给该 stream 的 `/raw` 订阅者。
关流对插件目前不可见（见「关流通知」），往死流写的帧被丢弃。
_Avoid_: 二进制通道、WebSocket 直连

**控制流（control stream）**:
协议 3 里 stream `0xFFFF_FFFF`（数据流是 `1 ..= 0xFFFF_FFFE`）。payload 是一个 ≤128 字节的
JSON 对象，目前只有 `{"op":"close","stream":N}`，双向、幂等：宿主在订阅者离开（或插件往死流
写数据）时发 close，插件用 `Streams::close` 发 close 让宿主把该 WebSocket 干净关闭（1000）。
非法控制帧被忽略并计数，不占 stream 表、不计入 raw 数据字节。
_Avoid_: 关流通知、svcd.stream.*（那是没落地的早期设想）

**发布钥（Release key）**:
仓库 CI 独占的 Ed25519 私钥，是打进 catalog 的唯一签发者；贡献者的自签钥只用于本地自测。
_Avoid_: 签名证书、管理员密钥

**catalog（索引）**:
描述"有哪些插件、哪个版本、支持哪个平台"的静态 JSON 树，是商城与管理器共同的唯一数据源。
_Avoid_: 注册表、插件表、marketplace API

**平台（platform）**:
源清单里的目标三元组，必须与宿主编译期三元组精确相等；一个包只服务一个平台。
_Avoid_: 架构（arch 只是三元组的首段）、系统

**门禁（Gate）**:
一个 PR 从"能构建"变成"可发布"必须通过的检查集合；签名发生在门禁之后，所以门禁就是信任边界。
_Avoid_: CI、流水线

**期（Phase）**:
一次交付范围的封口；一期只覆盖三个平台与双仓库的最小改动。
_Avoid_: 版本、里程碑
