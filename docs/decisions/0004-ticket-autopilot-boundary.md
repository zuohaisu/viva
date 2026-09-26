# ADR 0004 — Ticket Autopilot 的边界：从 North Star 到 Delivery Automation 子系统

Status: Accepted · Date: 2026-09-27 · Decider: Haisu（owner 显式指令）· 详表：`docs/architecture/viva-transition.md` §2/§13

> 效力说明："Ticket Autopilot 降级为 Delivery Automation 子系统、能力保留、不再是 North Star、不做大重构"是 Haisu 任务书中的**显式指令**；§3 的"思想晋升清单"与 §6 的目标态形态是产品定义轮的提案，随本 ADR 的边界生效但细节可议。

## Context

Ticket Autopilot（Plane-first 交付闭环：worktree 隔离 → Developer → deterministic checks → 独立 QA → 有界修复 → owner 授权 → 本地 commit）是本仓库的历史主体，能力真实、测试在案（`src/ticket_autopilot/`，~8.3k 行）。产品边界变化后它不再适合作为 North Star，但"删除或重写"既浪费也不诚实——它是 Viva 第一个被证明的能力。

## Decision

1. Ticket Autopilot **整体保留**，重新定位为 Viva 的 **Delivery Automation** 子系统：Resident/Task 可挂载的一种结构化自治工作流，不再是产品前门。
2. `src/ticket_autopilot/` 不 rename、不大改；`docs/closed-loop-workflow.md` 继续作为其权威操作定义；其 localhost Web UI 保留为子系统操作面。
3. 五条设计思想晋升为 Viva 全局纪律：evidence-first / no false success、actor-aware authority、bounded autonomy、append-only evidence + disposable environment、constrained invocation（见 viva-transition §13.1）。
4. 反清单：Plane schema、ticket contract/readiness、QA verdict 字段、one-active-Run 节奏、Plane-first 信息架构**永不进入 Viva core**（viva-transition §13.2）。
5. 未来方向（不承诺时间表）：Delivery Automation 作为 Task 的一种结构化 workflow 并入 workspace 语境，run evidence 并入 Episode。

## Consequences

- 对 Ticket Autopilot 的新功能开发冻结在新边界内，除非 owner 显式开票。
- 北极星变更已记录于 logs/goal-drift.md（accepted reprioritization）；goal check 机制对两个子系统分别有效（AGENTS.md）。
