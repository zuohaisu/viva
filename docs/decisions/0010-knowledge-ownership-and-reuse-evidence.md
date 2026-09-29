# ADR 0010 — 知识归属与复用证据

Status: **Accepted**（Haisu 显式指令，2026-09-27）· 相关：ADR 0006（多成员）、`docs/architecture/temporal-model.md`

> 效力说明：**"本轮至少保留真实工作证据及正确归属"是 Haisu 的显式要求**；四类知识的归属边界与"复用证据"的定义是实现选择，随使用演进。

## Context

旧文档已经区分了 experience / memory / self-model，但没有回答"多成员协作中，一条知识属于谁、给谁看"。归属错了的两种代价都很具体：把项目约束记进某个成员的个人记忆，换人就丢；把某个成员的习惯写进项目知识，所有人被迫继承。

另一个风险是把"记录"当成"成长"：写了 50 条条目不等于学到了东西。

## Decision

1. **四类知识，各有唯一 owner**：

| kind | owner | 含义 | 谁能看到 |
| --- | --- | --- | --- |
| `personal_memory` | 一个成员 | 该成员的长期事实/偏好（跨项目成立） | 该成员被指派的任务 |
| `self_model_candidate` | 一个成员 | 关于该成员的**假设**（带证据，永不自动晋升） | 同上 |
| `project_knowledge` | 一个 Project | 项目事实与约束（删掉成员仍为真） | 该项目的任务 |
| `team_knowledge` | 团队 | 协作方式、跨成员约定 | 全部任务 |
| `skill` | 团队（可绑定项目） | 可复用程序，写成 SKILL.md，可移植到任何读该格式的工具 | 全部任务 |

2. **无 provenance 不准入**：条目必须指向产生它的 task / execution / 明确来源，否则拒绝写入。
3. **只有被后续工作使用过，才算复用证据**：`viva knowledge use <entry> --execution <id>` 追加一条 usage 记录；`viva knowledge reuse` 只列出有 usage 的条目。**没有 usage 的条目不算复用证据**，UI/报告不得把它说成"已学习"。
4. **条目只追加、可撤回、可修正**：撤回写一条 retraction 记录（保留原因），不物理删除。
5. **存储是本机开放格式**：`~/.viva/knowledge/entries.jsonl` + `skills/<id>.md`，人可读、可备份、可迁移。
6. **本轮不实现自动反思与 Self-Model 演化**：`self_model_candidate` 只是记录+证据，没有任何晋升机制；完整反思循环、模型自我改写、Jev 决策辅助集成**明确记为后续范围**，不用假数据或 UI 文案模拟。

## Consequences

- "成长"只有在真实使用发生后才可被证明，报告里区分"已记录"与"已复用"。
- 项目知识可以随仓库走（如果 owner 选择把 `project_knowledge` 落到 repo 文件），Viva 只要求边界正确，不强制存储位置。
- 多成员共享时，团队知识是唯一默认共享层；个人记忆不会泄漏给其他成员的任务。
