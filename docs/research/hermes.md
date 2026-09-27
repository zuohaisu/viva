# Research — Hermes（Nous Research）的数据模型与成长机制

Status: research note · Date: 2026-09-27 · 来源：本机安装（Hermes.app, `com.nousresearch.hermes` v0.17.6）+ 官方文档

> **状态说明（2026-09-27 修订后）**：本文是上一轮（单 Resident 框架）的研究笔记，其中的 Q 编号与对象命名已被 `docs/decisions/0006`–`0010` 修正；研究结论本身仍然有效，与当前产品模型冲突处以后者为准。


本文回答：**Hermes 的 "agent 越用越强" 是怎么实现的；Viva 对 Hermes 采取什么策略（Q8）。** 决策本体在 `docs/decisions/0005-hermes-integration-strategy.md`。

> **后续核查（2026-09-28）**：Holographic 的公开源码、检索/反馈机制与接入缺口见 [专项研究](hermes-holographic-memory-2026-09-28.md)。本文的本机观察未在本轮重验，关于 Samuel 宿主的阶段性倾向以 [ADR 0011](../decisions/0011-rust-host-and-tui.md) 已批准的 Pi 方向为准；ADR 0005 仍是 Hermes 集成选项提案，不是批准结论。

## 1. 产品定位

Hermes 是 Nous Research 的开源（MIT）本地个人 agent，口号 "The Agent That Grows With You"——"one agent, one memory, every surface"：同一 agent 经 Telegram/Discord/Slack/Email/CLI 可达，跨会话记得你、从协作中学技能、可执行计划任务。Electron 壳 + 本地 Python 后端（`~/.hermes/`），可选付费云托管。

## 2. 数据模型（本机验证，全部是明文文件 + 标准 SQLite，可外部读写）

- **Profiles**：命名 agent 实例（本机有 `victor`、`oliver`、`ops-agent`），每个是完整隔离的家：自己的 `state.db`、sessions、memories、workspace、`config.yaml`。**注意：一个 profile ≈ 一个独立人格，不是同一个人的不同面。**
- **Memories**：有界、策展的 Markdown——`MEMORY.md`（agent 自我笔记，~2,200 字符硬上限）+ `USER.md`（user model：偏好、沟通风格，~1,375 字符上限）。会话开始时作为**冻结快照**注入 system prompt；agent 通过 add/replace/remove 自编辑，**硬上限强制整合（consolidation）**——这就是它成长循环的核心。`SOUL.md` 是身份/人格文件（system prompt 第一项）。
- **Skills**：程序性记忆——`~/.hermes/skills/<name>/SKILL.md`（YAML frontmatter + When to Use / Procedure / Pitfalls / Verification），兼容 **agentskills.io 开放标准**；由 agent（`skill_manage`）和 `/learn` 自动创建；后台 **curator** 进程整理技能，`.curator_ledger.jsonl` 记录每次变更的前后 sha256 与会话证据。技能可从 GitHub / Skills Hub 安装（`hermes://skill/install`）。
- **Sessions**：SQLite（`state.db`: `sessions`, `messages`, FTS5），`session_search` 工具可查；`/journey` 可浏览学习图谱。
- **Memory store（结构化）**：`memory_store.db`——`facts` 表带 `category`、`tags`、**`trust_score`**、`retrieval_count`、`helpful_count`、`hrr_vector`（全息约化向量）；entities 与 fact 关联。信任分 + 使用计数是量化的"越用越可信"机制。

## 3. 成长机制的拆解（Viva 真正要研究的东西）

| Hermes 机制 | 本质 | 对 Viva 的启示 |
| --- | --- | --- |
| MEMORY.md/USER.md 硬上限 | 记忆必须挣得位置（consolidation pressure） | Viva memory 也要有界 + 准入门槛；"记住更多文字"≠成长 |
| SKILL.md 程序性记忆 | 会做 ≠ 记得；可执行、可检验的程序另立一类 | Viva Skill 独立于 Memory；**直接采用 SKILL.md 格式获得生态互通** |
| Curator + ledger | 后台整理，变更留证 | Viva 的 reflection/策展步骤应有同等 provenance |
| trust_score / helpful_count | 记忆按"被用且有用"加权 | Viva memory 晋级/衰减可借鉴使用信号 |
| SOUL.md = 身份 | 身份 = 一份静态人格文件 | **Viva 不照搬，但先分清概念**：self-model 理论严格区分 **Self**（territory）、**Self-Model**（认知层：假设制、evidence-grounded、可修订）、**Identity commitments**（规范层："我选择成为什么"）。SOUL.md 的问题不是"身份存在"，而是它把规范层与整个人格压成一份不可修订的静态档案，且整个体系没有独立的假设制 Self-Model 层。Viva 的对应做法：Identity commitments 可以有（独立条目类），Self-Model 独立成假设系统，两者都不与 memory 混装（见 self-model 研究 §2） |
| Profiles 隔离 | 一个 profile 一个家 | 对应 Viva 的 "Resident 拥有自己的状态存储"，但 Viva 的 Samuel 要能跨 workspace 工作，不是单仓隔离 |

## 4. 可复用性评估（对 Viva）

- **导出/访问性：极高**。memories 是固定路径的可读 Markdown；skills 是普通 SKILL.md 文件夹；会话是 FTS5 SQLite；`hermes journey --json` 可导出学习图谱。官方建议迁移机器时直接备份数据目录。
- **可驱动性**：`hermes` CLI（chat/cron/send/…）、本地 OpenAI 兼容 proxy、`hermes send` 消息注入、deep links、MCP 支持。→ **Hermes 可以像 Codex/Claude 一样被当作 Worker 驱动**（Phase 1 不做，仅记录可行性）。
- **作为 Samuel 的 runtime：当前倾向否决，未终审**。把 Hermes 当 "Samuel 的身体" 意味着 Samuel 的记忆被 2KB 上限的 MEMORY.md 和 SOUL.md 静态人格文件定义——那是一个 Hermes profile（如本机 victor/oliver），且 Viva 的核心资产（长期记忆/自我模型）被锁进另一个产品的 schema 与演化节奏。这是当前的主要顾虑，不是已证明的结论；完整的选项分析见 ADR 0005（Proposed）。

## 5. 结论（当前倾向，非终审）

产品定义轮的当前倾向是 **"格式互通 + 单向导入 + 可选 Worker"，暂不 "复用其 memory/skills subsystem 作为 Samuel 的本体"**：

1. Skill 格式对齐 SKILL.md（agentskills.io）——技能可在 Hermes ↔ Viva 间流动；
2. Hermes 的数据（sessions、memories、journey）可被 Viva 只读导入为 Experience Journal 的历史材料；
3. Hermes 可作为未来的 Worker 选项之一；
4. Samuel 的记忆、user-model、self-model 由 Viva 自己拥有——**这是产品原则，但它不自动等于"每个组件都自建"**：Viva 拥有状态（自有格式、自有存储）与复用 Hermes 的某个成熟组件（如以其 skills 子系统为 Viva 技能层的实现载体、数据仍落在 Viva 自有存储）并不矛盾，该路径尚未被认真评估过。

哪些路径被否决、哪些仍开放、什么条件下重审——见 `docs/decisions/0005-hermes-integration-strategy.md`（Status: Proposed，待 Haisu 裁决）。

完整决策与理由：`docs/decisions/0005-hermes-integration-strategy.md`。

## Sources

- https://hermes-agent.nousresearch.com
- https://hermes-agent.nousresearch.com/docs/user-guide/features/overview
- https://hermes-agent.nousresearch.com/docs/user-guide/features/memory
- https://hermes-agent.nousresearch.com/docs/user-guide/features/skills
