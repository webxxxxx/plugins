# __NAME__ — 跨 OS 家族的脚本插件模板（Unix 一份、Windows 一份）

`script` 是给「一次要出 unix 和 windows 两种包、但不想写编译型语言」的项目用的：
同一组能力写两遍，Unix 用 POSIX sh，Windows 用 PowerShell，一个清单管两个平台。

## 为什么不是一份脚本

宿主的 manifest 只有**一套** `exec` / `interpreter`：`sh` 到不了 Windows，
`powershell` 在 unix 上叫 `pwsh`。所以 `plugin-tools template` 选了跨家族平台、
又选了 `script` 时，生成的是：

```
package/manifest.json          平台无关：id/name/version/desc/protocol/ui/capabilities
platforms/<name>.sh            Unix 侧脚本（提交）
platforms/<name>.ps1           Windows 侧脚本（提交）
platforms/lib/svcd-plugin.sh   Unix 侧的 SDK 副本（提交）
platforms/lib/SvcdPlugin.ps1   Windows 侧的 SDK 副本（提交）
platforms/unix.json            unix 家族一份：interpreter + exec，指向 <name>.sh
platforms/windows.json         windows 家族一份：interpreter + exec，指向 <name>.ps1
```

清单按**家族**生成，不按平台：选了 5 个平台也是这 6 个文件。某个三元组真要不一样时，
再加 `platforms/<三元组>.json`，它压在家族之上。

SDK 也在 `package/` 外面，和用它的脚本放在一起：运行期它是 `payload/lib/…`，构建时才
按家族挑一份进去。所以 Linux 包里不会有 `SvcdPlugin.ps1`，Windows 包里也不会有
`svcd-plugin.sh`。

没有 `exec` / `interpreter` 的 `package/manifest.json` 单独不是完整包，这是有意的：
工具在所有平台都合完之后才交给宿主。

## 打包时会发生什么

```bash
plugin-tools build --platform x86_64-unknown-linux-musl
```

1. 把 `platforms/unix.json`（再叠 `platforms/<三元组>.json`，如果有）并入清单；
2. 把 `platforms/` 下**属于本平台家族**的脚本复制到 `payload/bin/`，
   `platforms/lib/` 下同家族的复制到 `payload/lib/`
   （`.sh` → unix，`.ps1` → windows）；
3. 打包。

所以 Linux 包里只有 `<name>.sh` + `svcd-plugin.sh`，Windows 包里只有
`<name>.ps1` + `SvcdPlugin.ps1`，另一侧从来没进过包。

## 两边必须对齐

`capabilities[]` 是两者共用的：`package/manifest.json` 里列了什么，
`<name>.sh` 的 `case` 和 `<name>.ps1` 的 `switch` 就都要有。
没有任何编译期检查，改一处就同步改另一处。

## 命令

```bash
plugin-tools template <name> --platforms x86_64-unknown-linux-musl,x86_64-pc-windows-msvc --lang script
plugin-tools build --platform x86_64-unknown-linux-musl
plugin-tools build --platform x86_64-pc-windows-msvc
plugin-tools check dist/__NAME__-0.1.0-x86_64-pc-windows-msvc.spk
```

`--ui web` 会带上 `package/payload/web/` 并在清单里写 `ui`；两个后端实现的是同两个能力。

## 旧项目迁移

早先生成的项目把 SDK 放在 `package/payload/lib/`，那是**每个平台都会带**的位置：Linux 包里
也会有 `SvcdPlugin.ps1`。把它挪到 `platforms/lib/` 即可（构建时按家族挑一份进
`payload/lib/`）：

```bash
mkdir -p platforms/lib
mv package/payload/lib/svcd-plugin.sh package/payload/lib/SvcdPlugin.ps1 platforms/lib/
rmdir package/payload/lib
```

## 边界

- 两边都是脚本，SDK 各一份：`platforms/lib/svcd-plugin.sh` 与
  `platforms/lib/SvcdPlugin.ps1`，构建时进 `payload/lib/`（由 `sdk/` 同步，勿手改）。
- Windows 侧需要 PowerShell：系统自带的 5.1 够用；unix 侧只需要 `sh`。
- 跨家族项目也可以选 `rust` / `c`（一个二进制管所有平台）；只选一个 OS 家族时，
  可选语言里会多出 `shell`（只能 unix）和 `powershell`（只能那一个家族）。
