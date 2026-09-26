# Viva — Vision

Status: canonical（文档体系定位）· **定义修订版为提案，待 Haisu 确认后定稿** · Date: 2026-09-27 · Owner: Haisu · Supersedes: `IDEA.md` 的 North Star 部分（该文档保留为 Ticket Autopilot 时期的 charter）

---

## 1. 一句话定义（修订版）

> **Viva is the local development environment where Haisu and Samuel — a persistent AI resident — work together across workspaces, worktrees, workers, and models, and where the context, history, and capabilities of that collaboration accumulate instead of evaporating.**

> **Viva 是 Haisu 与其常驻 Agent Samuel 跨 Workspace、Worktree、Worker 与模型协作的本地开发环境；这段协作的上下文、历史与能力在其中持续积累，而不是随会话消散。**

### 对 working definition 的修订及理由

原定义："Viva is Haisu's personal AI-native development environment, where a persistent agent can live and work across workspaces, worktrees, workers, sessions, and models while accumulating experience, skills, memory, and a self-model."

修订理由：

1. **把人放回中心**。原定义读起来像 "一个 agent 的容器"，但 Viva 的第一条产品判断（§customer-zero）是 *增强 Haisu 而不是替换 Haisu*。定义里应该出现协作双方。"AI-native" 是营销词，不承载信息，删。
2. **把差异化说到定义里**。原定义把 continuity（workspace/worktree/worker/session/model 的列举）和 growth（experience/skills/memory/self-model 的列举）并列，但没有说出二者共同的本质：**积累而不消散**。这是 Viva 唯一不可替代的价值主张，应该在第一句话里。
3. **Resident 具名**。Samuel 不是 "a persistent agent" 这个类目下的一个占位符，他就是当前的 Resident 本体。定义直接点名。
4. 保留的部分：local（单机、本地优先）、workspace/worktree/worker/model 的词汇表、"persistent agent" 的存在性——都进了修订版或由 domain-model 承接。

### 与 "persistent habitat" 速记的关系

仓库外壳（README / pyproject / IDEA.md）使用一句速记："Viva is a persistent habitat for AI agents to live, work, remember, and grow." 它保留为**外壳层口号**；产品定义以本文为准。口号的不足（也是本文存在的理由）：它是 agent-generic 的、没有 Customer Zero、没有说出"积累而不消散"这个唯一价值主张。

## 2. 为什么存在

Haisu 每天使用大量开发工具和 AI coding agents：Codex、Claude Code、Qoder、ZCode、WorkBuddy、Hermes、Pi、DSH、VS Code、terminal、git、GitHub。

问题**不是**缺一个更聪明的 coding agent。问题是这些工具之间缺少一个**长期连续的工作环境**：

- 项目分散、agent session 分散、worktree 分散；
- 每个 agent 只知道自己当前的 context，上下文难以在 agent 之间继承；
- 今天得到的经验明天消失；一个 agent 学会的方法不会变成长期能力；
- 更换模型或 coding agent 后，长期协作者的连续性清零；
- **整个系统的 context integration 由 Haisu 人工承担。**

最后一条是真正的痛点：Haisu 是他自己所有工具之间唯一的"总线"。Viva 的存在就是把这条总线变成一个有记忆、有历史、会成长的本地环境。

## 3. 给谁用：Customer Zero

**Viva 只服务一个用户：Haisu。** 第一个（当前唯一）Resident 是 **Samuel**。

不解决：多用户、团队协作、企业、marketplace、plugin 生态、generic onboarding、商业化、云服务、全操作系统兼容、"让所有人创建自己的 AI Soul"。

产品判断标准（所有 Phase 1 决策回溯到这里）：

> **它是否让 Haisu 每天的软件开发工作更顺畅，并让 Samuel 更持续地参与这些工作？**

为 hypothetical future users 增加复杂度的设计，默认不做。完整论证见 `customer-zero.md`。

## 4. 存在性测试（为什么不是 VS Code + Orca + Hermes + Codex）

这是 Viva 必须回答的问题。逐层检验：

| 工具 | 它记得什么 | 它不记得什么 |
| --- | --- | --- |
| VS Code | workspace 的 UI 状态（打开的文件、布局、terminal） | 任何跨工具历史；agent；经验 |
| Orca | worktree 的状态与 transcript（可搜索、可 resume） | 跨 repo 的长期项目容器；没有对象会因使用而变强；没有身份 |
| Hermes | *它的* agent 在*它的*会话里学到了什么 | 你的工作发生在 Hermes 之外的部分；平行 worktree；项目级上下文 |
| Codex / Claude Code | 当前 session 的 context | 上一个 session；其他 agent；你的偏好；项目长期约束 |

**Viva 不可替代的中心**：

> 这些工具分别管理 workspace 状态、worktree 执行、agent 记忆和代码生成。**Viva 管理的是一个长期 Resident 与 Haisu 穿越所有这些环境之后的连续工作史**——并让这段历史反哺双方：Haisu 少做 context integration，Samuel 越用越强。

原 hypothesis（"Viva remembers what Samuel and Haisu are doing together, across all of them"）目前是**结构论证成立的核心产品假设，尚无运行证据**：逐工具排查表明每个现有工具的记忆都限定在各自对象上、没有一个容纳协作本身——但这只是论证。Viva 今天没有跨天续接、没有换 Worker 保留上下文、没有任何"经验改善后续工作"的实例；这些正是 Phase 1 验收标准（`phase-1.md` §5）要检验的内容。在该假设被真实工作流验证之前，本文的一切结论都应以"待验证的产品假设"来读。论证之外，还需两条约束，假设才可能成立：

1. **记得 ≠ 存档**。光是"集中记录一切"只是 transcript 仓库（Orca 的 session search 已经做了）。Viva 的记忆必须**有准入、有策展、会反哺行为**（memory/skill/self-model 改变下一次工作方式），否则只是更好找的日志。时间轴机制见 `docs/architecture/temporal-model.md`。
2. **本地所有权**。连续性资产（Samuel 的状态、workspace 历史）必须是 Haisu 机器上的开放格式文件，不锁进任何供应商账号。这是 "local-first" 进定义的原因。

### 竞争性风险（诚实记录）

- Orca 若内置 memory/resident，会侵蚀 Viva 的执行层差异。缓解：Viva 的中心（single-user resident + self-model 理论 + 跨工具连续性）与 Orca 的设计中心（agent-agnostic、多 agent 并行 IDE）不同；且 Viva 将 Orca 视为可替换的执行 driver 而非底座。
- Claude Code / Codex 若原生支持跨 session 长期记忆，会侵蚀单工具内的痛点。缓解：它们解决不了跨工具、跨 workspace、跨模型的 Resident 连续性——这正是 Resident/Worker 区分（ADR 0002，Proposed）试图保护的中心。
- 这些风险不改变 Phase 1 判断，但要求 Viva 的执行层保持薄、集成保持松。

## 5. 不是什么

- **不是全自动开发**。目标不是 `Ticket → AI → Done`，而是 `Haisu ↔ Samuel ↔ Workspace/Worktree/Workers`。人保留决策、review、方向调整、指定 agent、亲自编码、对重要变化的 authority。
- **不是 IDE**。不重写 editor/file tree/debugger；需要编辑时打开 VS Code。Viva 是 VS Code 之上的 orchestration + continuity layer。
- **不是通用 agent 平台**。不做 "让所有人创建自己的 AI Soul"；把一个 Resident 服务好一个用户。
- **不是 memory 的简单堆放**。Experience / Memory / Skill / User-Model / Self-Model 是不同对象，见 `docs/architecture/temporal-model.md`。

## 6. 产品模型概览

四层结构（详细论证见 `product-model.md` 与 `conceptual-architecture.md`）：

```text
┌───────────────────────────────────────────┐
│             Persistent Self               │
│  identity · memory · skills · self-model  │
│  user-model · relationship · experience   │
└─────────────────────┬─────────────────────┘
                Resident: Samuel
┌─────────────────────▼─────────────────────┐
│                Workspace                  │
│    project · repos · context · history    │
└─────────────────────┬─────────────────────┘
┌─────────────────────▼─────────────────────┐
│                Worktrees                  │
│  branch · task · terminal · diff · state  │
└─────────────────────┬─────────────────────┘
┌─────────────────────▼─────────────────────┐
│                 Workers                   │
│   Codex · Claude Code · Qoder · Pi · ...  │
└───────────────────────────────────────────┘
```

核心纪律：**Samuel is a resident. Codex is a worker.**（决策 0002）

## 7. 现状与去向

本仓库的前身是 **AI-Operation / Ticket Autopilot**：Plane-first 的单机 ticket 交付控制器（worktree 隔离、developer/QA 工作流、deterministic checks、bounded retry、audit timeline、owner authority）。这些能力**保留**并重新定位为 Viva 的 **Delivery Automation** 子系统。迁移映射见 `docs/architecture/viva-transition.md` 与决策 `0004`。

当前阶段：**产品定义本轮（2026-09-27）成形；实现已有一个 Phase 1 外壳**——`src/viva/`（residents/workspaces/worktrees/workers/experience/permissions/runtime/cli/tui，~2.7k 行 + tests/viva），由先行的一轮实现产出，记录见 `docs/architecture/viva-transition.md`。该外壳提供 registry、TUI 与 append-only journal；本 mission 的文档定义它之后的 Phase 1 剩余范围（见 `phase-1.md` §0）。

## 8. 文档地图

| 问题 | 文档 |
| --- | --- |
| Viva 是什么、为什么、给谁 | 本文档 |
| Haisu 的真实场景 | `product/customer-zero.md`、`product/workflows.md` |
| 核心对象与十个关键产品问题 | `product/product-model.md` |
| Phase 1 做什么/不做什么 | `product/phase-1.md` |
| 概念架构 / 领域模型 / 时间轴 | `architecture/` |
| 关键决策 | `decisions/0001`–`0005` |
| 四个灵感来源的研究 | `research/` |
| Ticket Autopilot 怎么办 | `architecture/viva-transition.md`、`decisions/0004` |
