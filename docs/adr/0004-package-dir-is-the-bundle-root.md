# 打包根是 `package/`，源码必须留在它外面

打包器（`plugin bundle`）无差别递归目标目录，**没有任何忽略规则**：它不接受 `.gitignore`、不支持排除列表、拒绝软链。任何一个插件目录如果同时装着源码和包内容，`src/`、`Cargo.toml`、`target/`、占位文件就会被原样打进 `.spk`。

所以每个插件都是两层：`plugins/<name>/package/` 是唯一打包根（里面有什么，包里就有什么），源码、构建配置、工具都放在它旁边。Rust 的产物由 `cargo build` 复制进 `package/payload/bin/`（git 忽略），Shell 的脚本本身就是包内容（提交）。

## Considered Options

- **给打包器加忽略规则**（`.spkignore`）：那等于在验证器之外再造一套"哪些文件属于包"的真相，而 `files[]` 已经是权威清单；两套规则迟早不一致。
- **打包前 staging 到 `dist/<name>/`**：可行，但多一次拷贝、多一个"staging 是否新鲜"的失效模式；`package/` 让"包里有什么"用 `ls` 就能看见。
- **整个插件目录就是包**：用 Python/Node 这类源码即 payload 的语言时最顺手，但 Rust/Go/C 的构建产物与源码同目录时必然污染。

## Consequences

- 占位文件也不能放：空目录在 git 里不存在，靠构建创建，别放 `.gitkeep`——它真的会被打进包里。
- 打包输出必须在打包根之外（打包器自己也这么要求），所以产物落在 `<plugin>/dist/`。
- 新增语言模板时，只要遵守"`package/` 是包、外面是源码"，构建步骤怎么写都行。
