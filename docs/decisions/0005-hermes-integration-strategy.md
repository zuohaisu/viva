# ADR 0005 — Hermes 集成策略（Proposed，待 Haisu 裁决）

Status: **Proposed** · Date: 2026-09-27 · 研究依据：`docs/research/hermes.md` · 本轮状态：**不受 2026-09-27 产品方向变更影响**（未实现，也未被取代；Hermes 既不是本轮依赖，也不是本轮范围）

> 来源与效力说明：Haisu 在任务书中明确要求"研究并讨论 Hermes 是 Worker 还是能力来源，不要预先假设答案，形成 ADR-style decision note，不要现在集成"。本文遵循该要求给出**选项分析与当前倾向**；倾向本身是产品定义轮的提案，**未经 Haisu 接受，不构成决定**。

## Context

Hermes（Nous Research，本机安装）是一个成熟的"越用越强"个人 agent：profile 隔离、MEMORY.md/USER.md 有界策展记忆、SKILL.md 技能体系 + curator、trust-score 事实库、可搜索会话。任务书列出五种可能关系：(1) 当作 Worker；(2) 直接复用其 memory/skills subsystem；(3) 借鉴设计但不依赖；(4) 作为 Samuel 的 runtime；(5) 混合。

关键事实（本机验证）：Hermes 数据全部是明文文件 + 标准 SQLite，可外部读写；skills 兼容 agentskills.io 开放标准；记忆有 ~2KB 硬上限；身份 = 静态 SOUL.md 人格文件；一个 profile ≈ 一个独立人格实例。

## 选项分析（诚实版，含尚未走完的评估）

| 选项 | 评估 | 当前判断 |
| --- | --- | --- |
| (1) 当作 Worker | `hermes` CLI 可驱动、本地 proxy 可用；与 Codex/Claude 并列即可 | **低风险，倾向采纳**（Phase 1 不做） |
| (2) 复用其 memory/skills subsystem | **评估未走完**。两条子路径不同：2a——Viva 的技能层直接以 Hermes 的 skills 子系统为**实现载体**（数据仍落 Viva 自有存储、Viva 拥有格式与生命周期）：可获得成熟 curator/ledger 机制，代价是接受其 SKILL.md 之上的行为约束，且耦合其演化节奏；2b——直接使用其 memory_store（trust_score/hrr 向量 schema）作为 Samuel 记忆存储：与"状态自有、假设制记忆"的产品原则冲突明显。**2a 未被认真评估过，"保持状态所有权"并不自动排除它** | 2b 当前倾向否决；**2a 保持开放，待 Viva 自建技能层遇到真实成本时评估** |
| (3) 借鉴设计但不依赖 | consolidation 压力、trust/helpful 信号、curator provenance 均可借鉴 | **倾向采纳**（已反映在 temporal-model） |
| (4) 作为 Samuel 的 runtime | 主要顾虑：Samuel 的记忆被 2KB 上限与 SOUL.md 静态人格定义——那是一个 Hermes profile，不是 self-model 理论中假设制、可迁移的 Samuel；核心资产被外部产品的演化节奏绑架。**这是当前最强的顾虑，但它是论证，不是被运行证据证明的结论** | **当前倾向否决**；若未来 Hermes 提供可自持有的记忆扩展点，可重审 |
| (5) 混合 | (1)+(3) 立即可做；2a 与 (4) 的边界是真正的开放问题 | **当前倾向**：格式互通（SKILL.md）+ 单向历史导入 + 可选 Worker；暂不 runtime 化 |

### 概念澄清（本轮修正）

此前文档把"SOUL.md 静态人格"直接对比"假设制 self-model"，混淆了 self-model 理论中三个不同的层：**Self**（territory）、**Self-Model**（认知层，假设制/evidence-grounded）、**Identity commitments**（规范层，"我选择成为什么"）。准确的批评是：SOUL.md 把规范层与整个人格压成一份静态档案，且 Hermes 体系没有独立的假设制 Self-Model 层——而不是"身份不该存在"。Viva 的对应做法：Identity commitments 可以有（独立条目类），Self-Model 独立成假设系统，两者都不与 memory 混装。

## 当前倾向（Proposed，非决定）

格式互通 + 单向导入 + 可选 Worker；2b 与 (4) 当前倾向否决；**2a 保持开放**。Viva 自建 memory/skill 存储接受重复建设成本，换取所有权与理论一致性——这笔交换是否值得，应 Phase 1 末用真实成本复核。

## 重审触发条件（写明，避免过早锁死）

1. Viva 自建技能/策展层的维护成本明显超出预期；
2. Hermes 的 skills 子系统提供可自持有的存储扩展点（数据落点可完全由 Viva 控制）；
3. Haisu 的工作流实际大量发生在 Hermes 内，单向导入不足以维持连续性。

任一触发即重新打开本决策。
