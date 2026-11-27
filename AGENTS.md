## Agent skills

### Issue tracker

Issues and specs live in this repo's GitHub Issues, driven by the `gh` CLI. See `docs/agents/issue-tracker.md`.

### Triage labels

Default five triage roles: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: one root `GLOSSARY.md` plus ADRs in `docs/adr/`. See `docs/agents/domain.md`.

## 工作副本

`research` 子代理在这个工作副本里干活，会切到自己的 `research/<name>` 分支并留在那儿。父会话每次 `git add` / `git commit` 之前先跑一次 `git branch --show-current`，确认自己在 `main` 上；子代理结束后也要检查 HEAD 是否回到了 `main`。

## 仓库结构

- `sdk/rust`：协议壳 crate `svcd-plugin`；`sdk/c`：单头文件壳；`sdk/shell`：POSIX sh 壳；`sdk/powershell`：PS 壳；`sdk/web`：浏览器 toolkit（无构建步骤）
- `samples/{hello,hello-c,hello-shell,hello-powershell}` 是四份等价的手写参考实现（同一组五能力），不要手改模板来迁就它们
- `tools/cli` 是 Rust 工具链 `plugin-tools`（单二进制）：`template` 建项目、`build` 构建打包、`dev` 装并启动、`devkey`、`sync`、`check`
- `tools/cross-env.sh` 是三元组到交叉工具链的唯一表；`tools/pack.sh` 在 docker 里打包（给 `build.sh` 的信号是 `SVCD_DOCKER=1`）。改构建脚本前先看这两个，别自己拼 `CC`/`AR`
- `tools/tag.py` 是发布用的打 tag 脚本：`python tools/tag.py all patch` 发 `v*`，`python tools/tag.py <plugin> patch` 发 `<plugin>-v*`，顺带改版本文件和 `Cargo.lock`
- 每个插件的 `package/` 是唯一打包根：打包器不过滤任何文件，源码必须留在它外面

## 写插件

新增插件、改插件代码、改 `sdk/` 之前读 `skills/svcd-plugin/SKILL.md`：支持的语言、toolkit 用法、以及会让包直接失效的边界。
