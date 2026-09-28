# Research — Orca 的概念模型与 Viva/Orca 边界

Status: **Historical observation; product-boundary recommendations superseded 2026-09-28** · Date: 2026-09-27 · 来源：本机 Orca 安装（Orca.app）与其 `orca` CLI 版本化指南（`orca skills get orca-cli`）

> **状态说明（2026-09-27 修订后）**：本文是上一轮（单 Resident 框架）的研究笔记，其中的 Q 编号与对象命名已被 `docs/decisions/0006`–`0010` 修正；本机观察作为历史材料保留，产品边界建议已被2026-09-28新要求取代，不作为当前推荐。


本文回答两个问题：**Orca 把 worktree 做成了什么；Viva 与 Orca 的边界在哪里（Q10）。**

> 2026-09-28 修正：下文 §2/§4/§5 的产品边界建议已由[独立首版门槛](../product/first-usable-version.md)取代。公开源码与复用以 [新核查](orca-reuse-audit-2026-09-28.md) 为准，旧“完整/最好”措辞不是当前验收结论。

## 1. Orca 的概念模型（来自本机 CLI guide，非二手资料）

- **Repo**：一等对象（`repo list/add/show`），带 base ref 配置。
- **Worktree 是核心单位**，官方定义："Orca's tracked view of a repo checkout, **its metadata, terminals, browser tabs, and UI state**"。ID 是二元地址 `<repoId>::<worktreePath>`。
  - 携带：display name、comment（卡片上的短状态行）、workspace-status（`todo / in-progress / in-review / completed`）、terminals、lineage（parent worktree / folder context / `--no-parent`）、setup hooks。
  - 有 `worktree ps`（谁在哪儿跑着）、`worktree current`、按 id/name/path/branch/issue 的 selector。
- **Terminal 是 worktree 内的执行面**：create/read/send/wait/split/close；`terminal wait --for tui-idle` 能等待 Codex / Claude / OMP / Pi / Grok 等 agent TUI 就绪；`--agent claude|codex|pi|...` 在第一个 terminal 直接启动 agent。
- **Agent Session Search**：宿主机上的 agent 会话全文索引（可按 agent / path / 时间过滤），每条命中带 session 和 **resumeCommand**。注意：需要人手动开启，且**没有跨机器搜索**。
- **Handoff / Orchestration**：full handoff = 把一个 worktree + agent 的所有权整体交给另一个 agent；orchestration 提供 task DAG、dispatch、inbox/reply、decision gates、coordinator loop（有监督的协调，不是自治）。
- **其他**：automations（计划任务）、artifacts（分享 HTML/MD，发布默认关闭、只能人开）、内置 browser（scoped to worktree）、skills sharing。
- **Folder contexts**：可以把 worktree 挂在 folder context 下形成分组（`parent-worktree folder:<folderId>`）——这是 Orca 里最接近 "workspace" 的东西，但它是轻量分组，不是长期项目容器。

## 2. Orca 已经解决了什么（要诚实）

Orca 对 "worktree 是独立工作的执行环境" 的实现已经非常完整：worktree 生命周期、agent 启动、terminal 读写、TUI 就绪检测、状态语义（todo/in-progress/in-review/completed）、handoff、会话搜索、多 agent 协调。**Viva 在 Phase 1 不应重造这一层。**

## 3. Orca 结构性不解决什么

Orca 是 agent-agnostic、session-scoped 的 IDE。它刻意不拥有：

1. **Resident**：没有跨 worktree / 跨 repo / 跨工具持续存在、持续积累的身份。Orca 对工作的"记忆"是可搜索的 transcript，不是被策展的 experience / memory / skill / self-model。没有 user-model，没有关系。
2. **Workspace 作为长期项目容器**：单位是 repo → worktree；folder context 是分组不是容器——没有 project context、长期经验、未完成 intentions、跨月历史。
3. **时间轴**：Orca 记得 transcript 在哪，但没有任何东西决定"什么值得学"。Orca 里没有对象会随着使用变强。
4. **跨工具历史**：VS Code、Hermes、裸 terminal 里发生的事，Orca 不知道。

## 4. Viva 吸收 / 拒绝 / 集成

| 维度 | 决定 |
| --- | --- |
| Worktree = 执行环境 | **吸收概念**。Viva 的 Worktree 对象同样携带：目的、branch、status、session、dirty 与否、任务归属、可否清理（语义对齐 Orca 的 status/comment，但对象是 Viva 自己的） |
| Worktree 执行机制 | **不重造，优先集成**。Phase 1 用 git worktree 原语自管最小实现（复用本仓 `GitWorktreeService` 思路）；Orca 作为可选 driver（`orca worktree create/terminal ...`）留作集成目标，不是依赖 |
| Worker 启动/TUI 管理 | Phase 1 自建最小 worker session 管理（复用 `agent_catalog` / `agent_sessions`）；不复制 Orca terminal 编排 |
| Workspace vs folder context | Viva Workspace 是长期容器（context/history/experience/intentions），Orca folder context 保持为轻量分组；如果某天 Viva 通过 Orca 执行，folder context 只是 Viva Workspace 的一个执行映射 |
| 状态语义 | 吸收 `todo / in-progress / in-review / completed` 这类简单状态语义到 Viva Task/Worktree |
| Session search | 吸收"transcript 可搜索 + 可 resume"的思想；Viva 的 Experience Journal 是它的时间轴策展层，不替代原始 transcript 索引 |

## 5. 边界结论（Q10 的完整论证见 product-model.md）

> **Orca 是平行 agent worktree 的本地执行织物（execution fabric）；Viva 是让一个 Resident 带着历史穿越这些环境的连续性层（continuity layer）。**

Haisu 不只用 Orca 的原因：Orca 里没有任何东西记得 "Haisu 和 Samuel 一起做过什么"、没有对象会因此变强、没有 workspace 级长期上下文。反过来，Viva 不该做的原因也一样成立：Viva 不重做 worktree 执行、terminal 编排、handoff——那些 Orca 已经是最好的了。竞争性风险与验证见 `docs/product/vision.md`。
