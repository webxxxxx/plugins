# wireguard 用一个插件 + variants 覆盖 8 个包

WireGuard 要发 8 个包：7 个平台通用包（gotatun 用户态实现）+ 1 个 SR6110 变体（包内
`wireguard.ko`）。通用包要在任何有 `/dev/net/tun` 的 Linux 上装，变体只能装到内核/固件
代次匹配的 `sr6110`。同一个 `aarch64` 三元组因此要出两个包，内容不同、`compat` 不同。

## Considered Options

- **两个 plugin id**（`wireguard` + `wireguard-sr6110`）：否决。商店里出现两个条目、两套
  管理页面和 peer 存储，用户要猜该装哪个；同一份代码还要复制一遍。
- **一个包同时带 ko 和 gotatun，`compat.devices` 列所有设备**：否决。含 `.ko` 的包**必须**
  声明 `compat`（打包器硬规则，防止内核模块装到不匹配的固件），一旦声明，通用包就被锁在
  sr6110 上；不声明则模块可能装到别的内核代次。
- **构建时临时改 `package/manifest.json`**：否决。合并后的清单在 build 之前就算好了，
  改源文件只对没有 overlay 的路径生效；而且那是绕过打包器校验，不是表达差异。
- **每个设备一份 `.plugin.json`**：做不到。一个目录只有一个 `.plugin.json`，工具链按
  “平台 × 变体”展开构建单元。

## Decision

- `.plugin.json` 用 `variants: [{device, platforms}]` 声明设备专用包；每个平台照旧出一个
  通用包，变体**额外**出一个。`build --device <id>` 只打变体。
- 变体的 `compat.devices` 由构建注入那一份 staged manifest；base manifest 不写，通用包
  保持无 `compat`。
- 两个包共用同一份代码，后端由**包里带了什么**决定：`wireguard.ko` → 内核模块，
  `gotatun` → 用户态。不探测宿主是否碰巧加载了内核模块，行为不随宿主漂移。
- SR6110 的 `wireguard.ko` 是唯一预编译件，放 `vendor/prebuilt/<三元组>/` 并带
  `SHA256SUMS`；其余（`gotatun`、`wg`）都在打包前从 `vendor/` 源码编译。

## Consequences

- 发布矩阵从“插件 × 平台”变成“插件 × (平台, 变体)”；产物名仍是
  `<id>-<version>-<platform>[-<device>]`，设备后缀取该包 manifest 的
  `compat.devices[0]`。
- 签名 job 不能再用目录名当平台：同一平台有两个包，device 后缀要按 manifest 剥掉。
- `--platform` 不带 `--device` 时只打通用包，保持老的“一个平台一个包”调用方式。
- macOS / Windows 只是往 `platforms` 里加三元组，变体机制不用动；它们的网络层是另一件事。
- 这条决策不改变 `plugin-tools` 对其它插件的语义：没有 `variants` 的插件，构建单元与
  以前完全一样。
