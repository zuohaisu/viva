# Viva — Domain Model

Status: canonical（词汇表结构）· **Proposed（关系裁决与不变量细节，待 Haisu 接受）** · Date: 2026-09-27 · 裁决出处：`docs/product/product-model.md` · 本文是概念对象的唯一定义处（不含实现 schema）

## 1. 对象图

```text
                        ┌─────────────────────────────┐
                        │ Resident (Samuel)           │
                        │  identity · memory · skills │
                        │  self-model · user-model    │
                        │  relationship               │
                        └──────┬──────────────┬───────┘
               observes/coor-  │              │  owns (continuity)
               dinates (非所有) │              ▼
                               │        ResidentSession
                               │        (Haisu↔Samuel 对话线程)
 ┌─────────────────────────────▼───────────────────────────┐
 │ Workspace (VicTrader / Viva / self-model / …)           │
 │  project memory · workspace skills · intentions         │
 │                                                          │
 │  ┌─ Repository (~/Projects/VicTrader)                    │
 │  │                                                       │
 │  ├─ Task ("处理 #812") ── may own ──┐                     │
 │  │                                  ▼                    │
 │  ├─ Worktree (issue-812) ◄─ anchored by ── WorkerSession │
 │  │    branch · status · dirty · purpose     (Codex @…)   │
 │  │                                            │          │
 │  └─ Episode ◄─ records ── Events ◄────────────┘          │
 │       (task · participants · outcome)                    │
 └──────────────────────────────────────────────────────────┘
```

## 2. 对象定义

### 空间/工作对象

| 对象 | 定义 | 关键属性 | 归属/生命周期 |
| --- | --- | --- | --- |
| **Resident** | 长期存在的 AI 协作者；Viva 中即 Samuel | identity（跨模型可迁移的开放格式状态）、memory 分区、skills、self/user-model 假设、relationship | Viva 全局；不随 session/workspace/模型消亡 |
| **Workspace** | 项目的长期容器 | name、repositories[]、project memory 指针、workspace skills、tasks、worktree 注册表、历史索引 | 长期；手工或半自动创建 |
| **Repository** | 一个 git 仓库的注册 | path、default branch、remotes | Workspace 拥有；一个 repo 原则上可被多个 workspace 引用（默认一对一，多引是例外需显式） |
| **Worktree** | 一个可独立工作的 checkout 执行环境 | path、branch、base、purpose、status（`active/archived/dirty/merged`）、linked task | Repository 派生；Task 可拥有；知情清理 |
| **Task** | 意图单元：一件要做的事 | intent、status（`todo/in-progress/in-review/blocked/done`）、linked worktree(s)、linked ticket(可选 Plane issue)、episodes[] | **Workspace 拥有**；可与 Delivery Automation Run 关联 |
| **Worker** | 一类可驱动的编码工具能力 | name（codex/claude-code/qoder/pi/hermes…）、invocation profile（CLI、permission modes、read-only 语义） | catalog 条目；可插拔、可探测 |
| **WorkerSession** | Worker 的一次执行 | worker、worktree（空间锚）、task（意图锚）、started_at、status（`running/exited/resumable`）、transcript 位置 | **隶属 Worktree、服务 Task**；短命可弃 |
| **ResidentSession** | Haisu ↔ Samuel 的对话线程 | workspace 语境属性、时间跨度、关联 episodes | Resident 拥有；跨 workspace 存续 |

### 时间/成长对象

| 对象 | 定义 | 关键属性 | 归属 |
| --- | --- | --- | --- |
| **Event** | 原子事实，append-only，secret-safe | type、时间、payload、来源（哪个 session/human） | Episode 关联 |
| **Episode** | 有边界的叙事工作单元（journal 的阅读/检索单位） | task、worktree、参与者（workers+Haisu）、outcome、event 引用 | 发生在 Workspace，由 Resident 记录 |
| **Reflection** | 对 episode 组的反刍记录 | 输入 episodes、输出（候选 distillate 或"无新证据"） | Resident |
| **Memory** | 策展过的长期事实 | content、scope（resident-global / resident-per-workspace / workspace-project）、provenance、使用信号 | 测试分流（见 Q3） |
| **Skill** | 可复用程序（SKILL.md 格式） | scope（resident / workspace）、verification、使用记录 | scope 分流（见 Q5） |
| **UserModel entry** | 关于 Haisu 的假设 | proposition、status、confidence、evidence、provenance | Resident |
| **SelfModel entry** | 关于 Samuel 自身的假设 | 同上 + supersedes | Resident；理论归 `self-model` 仓库 |
| **Run（Delivery Automation）** | Ticket Autopilot 的受管运行 | ticket、worktree、QA attempts、verdicts、owner actions | Task 的特化关联；evidence 并入 Episode |

## 3. 关系裁决（易混点集中回答）

1. **Resident 不拥有 Workspace、Workspace 不隶属于 Resident**。两者是多对多的"工作关系"：Samuel 在任何 workspace 里都是同一个 Samuel；workspace 的 project memory 不因 Samuel 消失（Q3 测试）。
2. **Task 是 Workspace 的孩子，不是 Worktree 的属性**（product-model Part I 修正 1）。Worktree 可以没有 Task（探索性）；Task 可以没有 Worktree（规划中）或多个 Worktree（方案对比）。
3. **WorkerSession 双锚**：Worktree（硬锚，决定副作用）+ Task（软锚，决定目的）（Q2）。
4. **两种 Session 严格分开**：WorkerSession 是工具的执行记录；ResidentSession 是协作的对话记录。绝不合用一张"session"概念（Q2）。
5. **Episode ≠ Session**：一个 episode 可跨多个 session（换 worker、跨天）；一个 session 可贡献多个 episode。Episode 以**目的**为界，Session 以**进程**为界。
6. **Memory/Skill/SelfModel 的归属由测试分流，不由层划分**：Q3（删掉 Samuel 仍为真？）、Q5（离开此项目仍成立？）。
7. **Run 不是 Viva core 对象**：它是 Delivery Automation 的领域对象，通过 Task 关联挂载（ADR 0004）。ticket/Plane 相关抽象不进入 core 词汇表。

## 4. 不变量（invariants，产品级）

- I1 Resident 状态永远可导出为开放格式，模型更换不改变状态语义。
- I2 Event/Episode 只追加；修正以新记录 + supersede 表达。
- I3 Memory 条目无 provenance 不成立（没有出处的"记忆"不准入）。
- I4 Worker/automation 无交付权：不 push 保护分支、不 merge、不自授权（actor-aware authority，沿用本仓 delivery_policy 语义）。
- I5 Worktree 清理必须知情：有未合并产出或未关闭 Task 的 worktree 不可被静默清理。
- I6 Self/User-model 的更新可见：候选 → 证据 → 晋升全程留痕，无静默身份改写。

## 5. 显式非对象（防止范围蔓延）

- **没有** "Project"（用 Workspace）；**没有** "Board/Backlog"（Task 列表足矣，票务归 Plane）；**没有** "Team/Member"（单用户）；**没有** "Plugin"（Worker/Skill catalog 是仅有的扩展面）；**没有** "Chat" 对象（ResidentSession 承载）；**没有**第五层。
