# ADR 0006 — 多成员 AI 协作：Resident ≠ Role ≠ Engine ≠ Worker ≠ Execution

Status: **Accepted**（Haisu 显式指令，2026-09-27）· Supersedes: ADR 0001 §Decision 1 的推论（"单人类用户 → 没有 AI Team/Member"）、ADR 0002 的"单一 Resident"框架 · Superseded by: —（本 ADR 为当前有效边界）

术语说明：此处统一产品称呼，不更改 2026-09-27 对多成员、身份与授权的裁决；下文引述为术语更新后的意译。

> 效力说明：**"Viva 是 Haisu 的本地优先个人 AI 协作系统，其中有多个持续存在的 AI 成员（Samuel/Oliver/Alice/Deven/Richard…），名字与职责都是配置数据"是 Haisu 的显式决定**。本文把它展开为对象关系与实现约束；展开细节（字段、状态布局）是实现选择，可随实现演进。

## Context

旧产品定义把 Viva 写成"Haisu 与**一个**常驻 Agent Samuel 协作的本地开发环境"，并把 multi-resident UX 与 AI 团队协作明确排除在范围外。真实形态不是这样：Haisu 的工作方式是让**多个**职责不同的 AI 成员长期存在并协作（PM/调度、开发、QA/review、运维、研究），同一成员同时参与多个任务。

旧定义的两个错误推论必须撤回：

1. "只有一个人类用户"⇒"没有 AI 团队/成员"——把 *客户数量* 与 *成员数量* 混为一谈；
2. 旧模型把 Resident 当作唯一持续对象，其余都是它的临时工具，于是无法表达"Deven 同时处理 Issue 150 与 151"。

## Decision

1. **Viva 是个人 AI 协作系统**：一个人类用户拥有多个持续存在的 AI 成员（Resident）。成员有各自的状态、历史、知识、技能与未来可演化的 Self-Model。
2. 对象关系固定为：

```text
Resident   持久身份（谁的连续历史）      —— 配置 + 历史 + 知识
Role       职责定义（用来做什么）        —— 配置数据，不写死
Engine     认知模型绑定（用哪个模型）    —— 可替换器官
Worker     执行工具（用哪个 CLI）        —— 可替换工具，catalog 数据
Execution  一次真实执行（这次发生了什么） —— 固定记录成员/任务/模型/工具/位置/授权
Task       意图（为什么做，跨执行存活）   —— Workspace 拥有
Workspace ≠ Project ≠ Repository ≠ Worktree ≠ Task（见 ADR 0009）
```

3. **一个成员可以同时参与多个任务**：成员身份相同，执行会话与任务上下文隔离；可写工作位置按任务隔离（独立 worktree）。
4. **成员名字与职责是配置数据**：`Samuel`/`Deven`/`Alice` 是记录，不是代码分支。角色目录（`~/.viva/config/roles.json`）与模型目录（`engines.json`）可由用户编辑。`Viva ≠ Samuel` 仍成立，只是不再等于"只有一个成员"。
5. **更换模型不删除成员状态与历史**：引擎是绑定字段；历史（append-only journal）与知识（带 provenance 的条目）独立存在。配置的模型或工具不可用时**明确报错，不静默替换**。
6. **诚实约束不变，且更强**：持久化记录只证明"配置、事件与知识被保留"，不宣称"已证明完整的 Self 连续性"。Self-Model 演化本轮不实现。

## Consequences

- 需要 Task / Execution / Grant 三个新对象；`src/viva/` 相应新增 `tasks/`、`executions/`、`office/`、`projects/`、`knowledge/`、`github/`（实现映射见 `docs/architecture/viva-transition.md`）。
- TUI 与 CLI 必须按"成员/任务/执行"三层呈现，UI 的当前选择只影响导航，不再决定执行归属（见 `docs/architecture/domain-model.md` 不变量 I6）。
- 旧的"单 Resident 是导航目的地"结论作废：成员是常驻底座，Workspace/Project 是语境（ADR 0009）。
- 不再为"未来可能多用户"设计：多成员 ≠ 多用户；仍是单机、单人类用户、本地优先。
