# ADR 0003 — Workspace 是第一语境与项目知识的家

Status: **Proposed**（待 Haisu 接受）· Date: 2026-09-27 · 上游：`docs/product/product-model.md` Q1/Q3、`docs/research/vscode-workspace.md`

> 效力说明："primary object 选谁"与"project knowledge 属于谁"是 Haisu 任务书**提出的问题**（Q1/Q3）；本文给出的答案是产品定义轮的**提案**（含两层分储的归属测试），尚未经 Haisu 接受。

## Context

两个问题必须一次裁决：UI/导航的第一层是 Resident 还是 Workspace；project knowledge 属于 Workspace 还是 Resident 的 memory。参照系：VS Code 以 workspace 为长期工作的持久句柄（window 一次性、workspace 持久）；而把所有记忆塞进 resident 会让项目知识随 resident 死亡、被其他 worker 无法读取；把所有记忆塞进 workspace 会切碎 user-model、并让 worker 读取"如何与 Haisu 协作"这类不存在的文件。

## Decision

1. **Workspace 是导航与语境的第一层**：Haisu 的一天按项目组织；`viva` 的入口、recent 列表、TUI 主结构以 Workspace 为单位。Resident 是永远在线的底座（对话的另一方），其身份/记忆/自我模型是全局成长面板，不是每日导航的第一站。
2. **项目知识两层分储，归属测试裁决**："如果明天删掉 Samuel，这条知识仍为真且有用 → Workspace（project memory，尽量进 repo 文件，任何 worker 可读）；如果它依赖 Samuel 与 Haisu 的关系或 Samuel 的轨迹 → Resident memory（按 workspace 分区）。"
3. Workspace 携带：repositories、project memory、workspace skills、tasks、worktree 注册表、工作状态与历史索引（配置进文件、状态进本地存储——VS Code 两分法）。
4. Task 是 Workspace 的孩子，不是 Worktree 的属性。

## Consequences

- workspace 切换 = 语境切换：project memory 注入随 workspace 走，user-model 全局跟随 Samuel。
- Workspace 的 project memory 建议落在 repo 内文件（可版本化），使其在 Viva 之外也成立。
- 删除一个 Workspace 不影响 Resident；删除 Resident 不销毁 Workspace 的项目知识。
