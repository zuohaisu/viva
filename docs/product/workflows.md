# Viva — Workflows

Status: canonical · Date: 2026-09-27 · 素材：`customer-zero.md` 场景 S1–S12 · 验收对象：`phase-1.md`

本文定义 Viva 的北极星工作流和它支撑的 top workflows。每条 workflow 标注它对 domain model 的最低要求——这些要求就是 Phase 1 的验收线。

## 1. North-star loop（Phase 1 的闭环）

```text
open viva
  ↓
resume Resident (Samuel 上线：加载身份、记忆、待续线索)
  ↓
select Workspace
  ↓
understand current project state (worktrees / workers / 上次 episode / 未完成 intentions)
  ↓
select / create Worktree
  ↓
launch Worker
  ↓
work (Haisu ↔ Samuel 对话；worker 执行；可换 worker)
  ↓
review / switch Worker
  ↓
record Episode (本次工作进入 journal；结论进入候选 memory/skill)
  ↓
return later
  ↓
continue (S4：不问"我们做到哪了")
```

**如果这个闭环成立，Phase 1 就有价值。** 它的成立标准不是功能全，而是 S1/S4/S8 三个场景可真实发生：

- S1/S4：回到 workspace 时，连续性由 Viva 提供，不由 Haisu 重建；
- S8：换 worker 时，任务上下文随任务走，不从零开始。

## 2. Top workflows（按 Phase 1 优先级）

### W1 — Resume a workspace（← S1, S4）
进入 workspace 即见：repositories、main 状态、active worktrees（含 dirty/branch/归属）、仍在跑或可 resume 的 worker sessions、最近 episodes、未完成 intentions。
最小要求：Workspace 注册表 + Worktree 状态 + Session 注册表 + Episode 索引。

### W2 — Start work on an item（← S2）
"处理 #812" → Samuel 创建/选择 worktree → 绑定 Task → 以 workspace context + project memory + task brief 启动 worker → 产出与 worktree/branch 绑定。
最小要求：Task 对象 + worktree 创建/选择 + worker 启动协议（catalog + session 登记）+ context 注入模板。

### W3 — Parallel independent review（← S3）
同一 task、同一证据包，第二个 worker 独立作业；分歧与裁决入 episode。
最小要求：Task 级 evidence 打包（含此前结论）+ 多 session 并存 + episode 记录分歧/裁决。

### W4 — Switch worker mid-task（← S8）
Codex 卡住 → 换 Claude Code：worktree 不动，已试方案/失败原因/约束随 task 交接。
最小要求：Task 上下文与 worker 解耦（session 死，task 上下文活）。

### W5 — Session & worktree hygiene（← S9）
列出全部 worktree/worker session 的归属、状态、活跃度；知情的归档/清理。
最小要求：Worktree/Session 的 status 语义（active/archived/dirty/merged）+ 清理前置检查。

### W6 — Record & revisit（← S5, S11）
episode 收尾时 Samuel 产出 distillate 候选（memory/skill）；日后按 workspace/task/主题检索 episode 与结论（含出处）。
最小要求：Episode→Event 关联 + 候选 distillate 流程 + 检索。

### W7 — Grow (background loop)（← S6, S7）
复现的教训进入 memory；跨情境的行为模式成为 self-model/user-model 候选假设——Haisu 可见、可否决。
最小要求：准入门槛（temporal-model）+ 假设文件 + 反思记录。**Phase 1 允许手动触发反思，不要求自动。**

### W8 — Delivery automation run（← S12）
在 workspace 语境里发起 Ticket Autopilot 式结构化流程（worktree → dev → QA → 有界修复 → owner 授权）。
最小要求：Viva 的 Task 能挂载 Delivery Automation 作为一种结构化 workflow；run evidence 并入 episode。**Phase 1 允许此 workflow 仍走现有 Web 控制器，只要求对象（worktree/task/episode）能对上。**

## 3. 交互纪律（TUI 角色，本阶段只定义不实现）

- `viva` 进入 Claude Code 式 TUI；围绕 Resident / Workspace / Worktrees / Workers / Conversation / Task / Activity 组织，**不做** file tree / editor / debugger / extensions。
- 需要编辑文件 → 打开 VS Code（可由 Viva 按 worktree 代开）。
- 对话是交互模式；workspace 是语境框架；两者同时成立（见 product-model Q1）。
