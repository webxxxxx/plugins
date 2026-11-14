# yank 与信任撤销：一手证据与 catalog 启示

- 日期：2026-11-14
- 关联票：[#3 撤回与信任撤销：版本级与密钥级的关系](https://github.com/webxxxxx/plugins/issues/3)
- 问题：版本撤回（yank）与信任撤销（撤钥）是不是一件事？撤回后已安装用户怎么办？静态 JSON catalog 怎么表达？

## 结论

1. **不是一件事。** yank 是“目录不再默认推荐某个版本/制品”，信任撤销是“本机不再信任某个 keyId”。前者是发布侧的策展，后者是消费侧的安全策略。
2. **yank 不删除、不影响已安装用户。** Cargo 的原文最清楚：yank 只从 index 移除，数据仍可下载；已有 lockfile 与直接下载不受影响；`--undo` 可逆。npm 的 `deprecated` 也只是版本元数据里的一个字符串，tarball 还在。
3. **yank 不是密钥泄露的补救。** Cargo 明确说：凭证泄露时 yank 不能阻止已存在的 lockfile 或直接下载，因此不能阻止泄露扩散。密钥泄露要走撤销信任 + 带外通告。
4. **catalog 只该表达 yank，不该表达撤钥。** 撤钥需要独立于被撤销密钥的信任根；TUF 的根密钥撤销由仍受信任的阈值根密钥签新元数据，阈值根密钥被攻破时则必须带外更新。catalog 由发布钥/CI 控制，无法承担这个角色。
5. **粒度要按我们的分发单位适配。** Cargo 是 version-level（crate 平台无关）；我们的 `.spk` 是 platform-specific，安装单位是 `(name, version, platform)`，所以 yank 应落在制品条目上，而不是整个 version。

## 1. Cargo / crates.io 的 yank

### 语义

`cargo yank` 的官方描述：

> The yank command removes a previously published crate’s version from the server’s index. This command does not delete any data, and the crate will still be available for download via the registry’s download link. Cargo will not use a yanked version for any new project or checkout without a pre-existing lockfile...
>
> — https://doc.rust-lang.org/cargo/commands/cargo-yank.html

关键点：

- 只从 index 移除，不删除数据；下载链接仍可用。
- 新项目/没有 lockfile 的 checkout 不会选 yanked 版本。
- 已有 lockfile 不受影响；直接下载也不受影响。
- `--undo` 明确存在，yank 可逆。

### 对已安装/已锁定用户

官方在“凭证泄露”段落里把边界说得更直白：

> Yanking only prevents Cargo from selecting this version when resolving dependencies by default. Existing lock files or direct downloads are not affected, so yanking cannot stop further spreading of the leaked credentials.
>
> — 同上

这句话同时证明了两件事：yank 不碰已安装/已锁定用户；yank 不能替代密钥撤销。

### 索引里的字段

Cargo index 的每个版本行有一个布尔字段：

> // Boolean of whether or not this version has been yanked.
> "yanked": false,
>
> — https://doc.rust-lang.org/cargo/reference/registry-index.html

这是内联在版本记录里的布尔值，不是单独的撤回清单。Cargo 的粒度是 version-level，因为 crate 是平台无关的源码包。

### 对我们的启示

- 字段形状可以借：布尔 `yanked` + 可逆的 `--undo` 语义。
- 粒度不能照抄：我们的制品是 platform-specific，同一 version 有三个平台；照 Cargo 的 version-level 会误伤其它平台。应落在 `(name, version, platform)` 制品条目上。
- “不删除、可下载、已安装不受影响”应原样保留：yank 只改目录推荐，不碰 `.spk`、sha256、签名。

## 2. npm 的 deprecate 与 unpublish

### deprecate 是元数据，不是删除

npm registry 的版本元数据里有一个 `deprecated` 字符串。以 `request@2.88.2` 为例，同一个 JSON 里同时有：

- `dist.tarball`: `https://registry.npmjs.org/request/-/request-2.88.2.tgz`
- `deprecated`: `request has been deprecated, see https://github.com/request/request/issues/3142`

— https://registry.npmjs.org/request/2.88.2

这说明 deprecate 只是给版本加一条人类可读的提示，tarball 仍在，已安装用户继续用。官方命令文档：https://docs.npmjs.com/cli/v10/commands/npm-deprecate

### unpublish 是另一条路径

npm 把 `unpublish` 单独作为移除包的路径，并受官方政策约束：

- https://docs.npmjs.com/cli/v10/commands/npm-unpublish
- https://docs.npmjs.com/policies/unpublish

这与 deprecate 的“只写提示”是两件事。对我们的启示：catalog 里的 yank 更接近 npm deprecate，而不是 unpublish；不要因为 yank 就删除 `.spk` 或让旧 URL 404。

### 对我们的启示

- `yankedReason` 可以借 npm 的 `deprecated` 字符串：给人看，不承担机器语义。
- 状态与消息分开：`yanked: true` 是状态，`yankedReason` 是消息；不要把“是否撤回”编码进字符串。
- 不要用 yank 表达“这个包有毒/密钥泄露”；那是安全通告 + 撤钥的领域。

## 3. TUF 的密钥撤销

TUF 规范把根密钥撤销放在“密钥管理与迁移”里：

> If less than a threshold of Root keys are compromised, the repository should revoke trust on the compromised keys. This can be accomplished with a normal rotation of root keys, covered in section § 6.1 Key management and migration. If a threshold of Root keys are compromised, the Root keys should be updated out-of-band...
>
> — https://theupdateframework.github.io/specification/latest/ §2.1.1 / §6.1

要点：

- 撤销信任是签名元数据操作，由**仍受信任的**根密钥阈值签发；不是“被撤销的密钥自己说‘我撤销了’”。
- 如果阈值根密钥都被攻破，规范要求**带外更新**，因为此时没有任何在线元数据可信。
- 委托可以随时撤销：委托方签新元数据，不再包含该委托即可。

### 对我们的启示

- catalog 没有独立于发布钥的根。让 catalog 表达“撤钥”只有两种结局：不可信（能改 catalog 的人可以伪造）或循环（被攻破的发布钥签自己的撤销）。
- 一期撤钥只能走本机 `trust.d` + 带外通告；要做分布式撤销，需要 TUF 式的独立根与签名元数据，那是另一个 effort。
- catalog 可以带 `keyId` 事实字段，但不能带 `revoked: true` 这种策略字段；策略的真相在 `trust.d`。

## 4. 对 catalog 的启示

| 问题 | 建议 | 依据 |
|---|---|---|
| 放哪 | 每插件分片的制品条目内联 | Cargo index 内联 `yanked`；ADR-0003 静态 JSON + 原子重写 |
| 粒度 | `(name, version, platform)` | 我们的安装单位；Cargo 的 version-level 不适用于 platform-specific `.spk` |
| 字段 | `yanked` 布尔；`yankedAt`；`yankedReason` | Cargo 布尔 + npm `deprecated` 字符串 |
| 可逆 | 是，设回 false 并清掉时间/原因 | Cargo `--undo` |
| 删除 | 否；不删 `.spk`、不改 sha256、不重签 | Cargo 不删数据；npm deprecate 不删 tarball |
| index | 不复制 yank 状态；latest 由分片算或同事务重写 | 避免两处真相；ADR-0003 的原子重写 |
| 撤钥 | 不进 catalog；只走 `trust.d` + 带外通告 | TUF 的独立根要求 |
| 已安装用户 | 不推送、不自动升级/卸载；商城徽标 + 带外通告 | Cargo lockfile 不受影响；npm deprecate 不删包 |

## 5. 与票 #3 建议的对照

- Q1 制品级内联：**支持**。Cargo 的内联布尔可借，粒度按我们的 platform 制品调整。
- Q2 撤钥不进 catalog：**支持**。TUF 表明撤销需要仍受信任的独立根；catalog 不是。
- Q3 撤钥后已装包继续运行：**支持**，但这是“安装时验签、加载不重验”的现状，应写进已知限制；安全召回靠带外通告 + 管理员卸载。
- Q4 已 yank 仍允许直接 URL 安装：**支持**。Cargo 说直接下载不受影响；yank 不是阻断。
- Q5 只做商城徽标 + 带外通告：**支持**。管理器一期不读 catalog，也没有更新通道；加轮询/自动停用是新的安全面。

## 来源

- Cargo `cargo yank`: https://doc.rust-lang.org/cargo/commands/cargo-yank.html
- Cargo index format: https://doc.rust-lang.org/cargo/reference/registry-index.html
- npm `npm deprecate`: https://docs.npmjs.com/cli/v10/commands/npm-deprecate
- npm `npm unpublish`: https://docs.npmjs.com/cli/v10/commands/npm-unpublish
- npm unpublish policy: https://docs.npmjs.com/policies/unpublish
- npm registry example: https://registry.npmjs.org/request/2.88.2
- TUF specification: https://theupdateframework.github.io/specification/latest/
