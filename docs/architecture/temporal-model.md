# Viva — Temporal Model

Status: canonical · Date: 2026-09-27 · 上游：`conceptual-architecture.md` §1 · 理论来源：`docs/research/self-model-relationship.md`、`docs/research/hermes.md`

## 1. 为什么需要独立的时间模型

空间轴（workspace → worktree → worker）回答"在哪工作"；时间轴回答四个空间轴无法回答的问题：

```text
what happened before?   → journal（发生了什么）
what did we learn?      → memory / skill（学到了什么）
how have we changed?    → self-model / user-model（我们怎么变了）
what should persist?    → 策展与准入（什么值得留下）
```

VS Code 恢复状态（restore），Viva 还要恢复**意义**（recall）。"昨天做了什么"的答案不是 transcript 全文，而是：上次 episode 的 outcome、由此产生的结论、以及仍未关闭的 intentions。

## 2. 沉淀管线（temporal pipeline）

```text
           空间轴上的工作
                │  留痕
                ▼
   Events ──────► Episode(s)          [journal: append-only, 廉价, 海量]
                │     │
                │     ▼  Reflection（人工触发为主, Phase 1）
                │  候选 distillate
                ▼
   ┌────────────┼──────────────┬────────────────┐
   ▼            ▼              ▼                ▼
 Memory      Skill       User-Model 条目   Self-Model 条目
 [有界]    [SKILL.md]    [假设制]          [假设制, 慢层]
```

纪律：**左侧廉价且必须全记，右侧昂贵且必须挣得。** 记忆的堆量不是成长；能改变下一次行为的沉淀才是。

> **现状（诚实约束）**：已建成的外壳只有最左端——append-only 的 experience journal（`viva-transition.md` §9：journal 永不被称为 memory；未建成的能力不得在 UI 声称）。本文描述的是目标态与准入门；每一层建成前，产品对外的说法必须停留在实际已建成的层。

## 3. 准入门（什么该留下）

### Event → Episode（组织门）
Event 是机器的；Episode 以**目的**为界：一个 task 的推进、一次排查、一次 review。由参与/时间/outcome 划界，工具辅助、人可修正。

### Episode → Memory（Q6 的裁决）
四个触发器之一才可成为候选：**复现**（第二次需要同一教训）、**意外**（与既有信念冲突）、**强调**（Haisu 明说记住）、**高再推导成本**。候选经策展（改写为可复用的陈述 + 挂 provenance）才准入。Memory 有界：新条目进，旧条目按使用信号衰减。默认策略：**多记 experience，少授 memory。**

### Episode → Skill（Q5 的裁决）
准入门：**可复用 + 可验证**（下次能照做、做完能检查对错）。一次成功不自动成 skill；同一方法第二次奏效时捕捉（S5），或 Haisu 主动说"这个方法留下"。scope 分流与晋升规则见 product-model Q5。

### Episode → User-Model / Self-Model（Q7 的裁决）
门槛最高（self-model 的 update gradient；**具体数值是产品定义轮建议的默认值，待 Haisu 接受并经真实数据校准**）：
- 观察层（L0–L1）候选：同一模式出现在 **≥3 个不同 episode、≥2 个不同 context**；
- 以 hypothesis 形式记录：proposition / status（candidate→active→revised→superseded）/ confidence / supporting & contradicting evidence / supersedes；
- **inference ≠ mutation**：Phase 1 晋升 = Haisu 可见的评审动作，不存在静默身份改写；
- Value/Identity 层（L3+）只记录证据，不晋升。

## 4. 三种存储温度

| 温度 | 内容 | 特性 |
| --- | --- | --- |
| Hot | 当前 ResidentSession、active worktree/task 状态、活 worker sessions | 进 `viva` 即在场 |
| Warm | journal（events + episodes）、memory、skills、假设文件 | 可检索、带 provenance、定期衰减 |
| Cold | 归档 episodes、已 supersede 的假设、archived worktree 记录 | 不进日常上下文，可查证 |

## 5. Restore 与 Recall 的分工

| | Restore（空间轴） | Recall（时间轴） |
| --- | --- | --- |
| 问题 | "我上次在哪、开到哪" | "我们之前懂了什么、决定了什么、还欠什么" |
| 机制 | workspace/worktree/session 状态重放 | episode/memory/检索的上下文注入 |
| 对标 | VS Code workspaceState | （没有任何现有工具做对——这是 Viva 的差异点） |
| Phase 1 | 必做（S1/S4） | 最小版：最近 episode + 未竟 intentions + 相关 memory 注入 |

## 6. 衰减与反哺（环的闭合）

- 衰减：memory/skill 长期未被使用或引用后未被验证 → 降权 → 复审时降级/归档（借鉴 Hermes 的 trust/helpful 信号，门槛更严）。
- 反哺：distillate 改变行为的方式是**进入下一次工作的注入集**——project memory 与 resident memory/skill 是 Samuel 每次 task 启动上下文包的常规成分；假设影响默认提案策略（S6/S7）。
- 证据回流：self-model 理论目前没有日常运行证据；Viva 累积的假设史（什么被巩固、什么被证伪、梯度实际多陡）定期回流 `self-model` 仓库的研究文档（Viva 对理论层的唯一回馈义务）。
