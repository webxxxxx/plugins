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
- `templates/{rust,c,shell,powershell}` 是唯一模板源；`samples/hello-rust`、`samples/hello-shell` 由 `node tools/plugin-template.mjs ... --force` 生成，不要手改；`samples/hello` 是手写的参考实现
- `tools/plugin-template.mjs` 只建项目；`tools/plugin-build.mjs` 构建 + 打包；`tools/plugin.mjs` 提供 `devkey / dev / check / sync`
- 每个插件的 `package/` 是唯一打包根：打包器不过滤任何文件，源码必须留在它外面

## 写插件

新增插件、改插件代码、改 `sdk/` 之前读 `skills/svcd-plugin/SKILL.md`：支持的语言、toolkit 用法、以及会让包直接失效的边界。
