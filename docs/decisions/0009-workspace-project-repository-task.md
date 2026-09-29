# ADR 0009 — Workspace ≠ Project ≠ Repository ≠ Worktree ≠ Task

Status: **Accepted**（Haisu 显式指令，2026-09-27）· Supersedes: ADR 0003（"用 Workspace 取代所有 Project 概念"）

> 效力说明：**"强制用 Workspace 取代所有 Project 概念"已被 Haisu 明确取代**。新的边界是"这些概念需要清楚区分，但不要求每个概念立即拥有独立服务或复杂数据库"。归属规则（谁拥有什么知识）是本文的设计部分，随实现演进。

## Context

旧定义把 Workspace 抬成唯一语境对象，并明确写"**没有** Project"。结果是：一个 workspace 只能对应一个仓库目录，无法表达"Haisu 用 Viva 管理多个项目、一个项目跨多个仓库、一个仓库同时服务两个项目"，也无法区分"意图"（要做什么）与"执行环境"（在哪个 checkout 里做）。

## Decision

对象与包含关系：

```text
Workspace（长期工作语境，如 "开发协作"）
  └─ Project（一摊有名字的工作，如 "Viva" / "Hiring automation"）
       └─ Repository（一个 git 仓库；一个项目可引用多个，一个仓库也可被多个项目引用）
            └─ Worktree（一个可独立写入的 checkout，按任务分配）
Task（意图，属于 Workspace；可以跨多个 worktree / 多个执行；可以没有 worktree）
```

1. **Workspace**：长期语境与注册单位（本地目录/仓库的集合、project memory 指针）。
2. **Project**：Workspace 里的工作单元；拥有 repositories 列表。项目知识（project knowledge）挂在 Project 上。
3. **Repository**：真实 git 仓库路径；多对多关系必须显式声明（`viva project bind`），不靠目录嵌套推断。
4. **Worktree**：执行环境。可写任务（delivery/implementation/fix/chore）**必须**有自己的 worktree；研究/规划/review 任务只读运行在仓库里，不创建 worktree。
5. **Task**：意图对象，属于 Workspace（可关联 Project/Repository），**不是 worktree 的属性**：它可以先于 worktree 存在，也可以被多个执行服务。
6. **Worktree 归属 Viva，不落在仓库内部**：`~/.viva/worktrees/<repository>/<task-id>`，避免污染 owner 未选择忽略的仓库。

## Consequences

- `viva project add/bind/list` 与 `Task.project_id` 使"项目"重新可见；workspace 不再是项目的别名。
- 归属测试（谁拥有这条知识）因此可执行：personal → 成员、project → Project、team/skill → 团队（见 `docs/architecture/temporal-model.md` §4）。
- 一个任务一个 worktree 的规则让"同一成员并行两个任务"天然隔离（验收场景 1/2）。
- 仍然只做本地：多仓库、多项目都是本机路径，不引入远端项目概念。
