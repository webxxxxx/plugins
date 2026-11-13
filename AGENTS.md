## Agent skills

### Issue tracker

Issues and specs live in this repo's GitHub Issues, driven by the `gh` CLI. See `docs/agents/issue-tracker.md`.

### Triage labels

Default five triage roles: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: one root `GLOSSARY.md` plus ADRs in `docs/adr/`. See `docs/agents/domain.md`.

## 工作副本

`research` 子代理在这个工作副本里干活，会切到自己的 `research/<name>` 分支并留在那儿。父会话每次 `git add` / `git commit` 之前先跑一次 `git branch --show-current`，确认自己在 `main` 上；子代理结束后也要检查 HEAD 是否回到了 `main`。
