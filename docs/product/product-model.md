# Viva — Product Model

Status: canonical（结构）· **Proposed（裁决内容）** · Date: 2026-09-27 · 上游：`vision.md` · 下游：`architecture/domain-model.md`

本文回答两个问题：**Viva 由哪些核心对象组成（四层模型 + 修正）；十个必须回答的产品问题（Q1–Q10）。** 效力说明：四层结构与问题清单来自 Haisu 的任务书；**对每个问题的答案是产品定义轮的提案**，其中具体门槛数值（如 Q7 的 ≥3 episode / ≥2 context）是 agent 建议的默认值，均待 Haisu 接受后才成为决定。对象的完整字段与关系在 `architecture/domain-model.md`。

---

## Part I — 四层模型

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

三点修正（不增加新层）：

1. **Task 的位置**：Task 在原图里出现在 Worktree 层的携带物中（"branch · task · terminal"）。裁决：**Task 是 Workspace 拥有的意图对象**（workspace contains tasks），Worktree 可以被 Task 拥有（一对一或暂无），Worker session 服务于 Task。理由：一个 task 可以还没建 worktree（规划中），也可以跨多个 worktree（并行方案对比）；把它当 worktree 的属性会把这个关系弄反。
2. **两条轴不是同一种"层"**：空间轴（Workspace → Worktree → Session/Worker）是**包含**关系；时间轴（Experience → Memory → Skill → Self/User-Model）不是第五层，而是**附着在不同对象上的成长状态**——project memory 附着于 Workspace，experience/memory/self-model 附着于 Resident。见 `architecture/temporal-model.md`。
3. **层与层的关系是"附着"而非"调用"**：Workers 不"调用" Workspace；Workers 在 Worktree 里执行，被 Session 记录，由 Resident 观察与协调，一切沉淀进时间轴。这决定了 Viva 的 orchestration 是松的、可替换的（integrate, don't clone）。

## Part II — Resident 与 Worker（严格区分）

| | **Resident（Samuel）** | **Worker（Codex / Claude Code / Qoder / Pi / Hermes…）** |
| --- | --- | --- |
| 存在方式 | 长期存在，跨 session/workspace/模型连续 | 按任务启动，可停止、可更换、可升级、可失败、可并行多个 |
| 拥有 | history、experience、memory、skills、self-model、user-model、relationship | 一个 invocation：cwd（worktree）、permissions、一次 session |
| 失败语义 | 不"失败"，只积累教训 | 失败是常态，被记录后替换即可 |
| authority | 提案者/协调者；不拥有交付权 | 无交付权（不 push/merge/自授权——沿用 actor-aware 原则） |
| 换掉它 | = Samuel 换认知 substrate（状态必须可迁移；self-model 证伪测试） | = 换工具，上下文由 Task/Episode 交接 |

目标态的工作分工：

```text
Samuel asks Codex to implement.
Samuel asks Claude to review — with the same evidence, independently.
Samuel compares results and remembers the decision.
```

真正持续积累历史的是 Samuel。此裁决的完整 ADR：`decisions/0002-resident-vs-worker.md`。

## Part III — 十个产品问题（Q1–Q10）

### Q1 — Primary object：UI/导航第一层是 Resident 还是 Workspace？

**裁决：Workspace 是导航第一层；Resident 是永远在线的底座，不是导航目的地。**

理由：
- Haisu 的一天按项目组织（VicTrader / Viva / self-model…），不按"去探访 Samuel"组织。VS Code 的教训：workspace 是 "the thing I'm working on" 的持久句柄（见 `research/vscode-workspace.md` §2）。
- Samuel 的工作天然 rooted 在项目语境里。Chat-first（Resident-first）的界面会把 workspace/worktree/worker 变成二等公民——这正是 Claude Code 类工具的局限。
- 但交互方式是对话：`viva` 打开的 TUI 就是在 workspace 语境里与 Samuel 对话。**Workspace 回答"在哪工作"，Conversation 是交互模式，Resident 是对话的另一方——三者不冲突。**
- Resident 仍然有自己的全局视图（身份、记忆、技能、自我模型、跨 workspace 关系），那是**设置/成长面板**，不是每日工作的第一站。

### Q2 — Worker session 属于谁？

**裁决：需要关系模型，且只有两个真归属：空间锚 = Worktree，意图锚 = Task。Resident 是观察者，不是所有者。**

- **WorkerSession 隶属 Worktree**（在哪跑：cwd/branch/terminal 决定它的一切副作用）且**服务一个 Task**（为什么跑）。Task:Session 是一对多（并行 review、换人重试）；Worktree:Session 一对多（先后多个 session）。
- Resident ↔ Session 是 **观察/协调关系**（session 的产出流入 Resident 的 experience），不是所有权。这与 0002 决策一致：worker 的上下文归任务，成长归 resident。
- 必须区分两种 session（domain-model 里分开建模）：
  - **WorkerSession**：工具的一次执行，短命、可弃。
  - **ResidentSession**：Haisu ↔ Samuel 的连续对话线程，跨 workspace 存在（带当前 workspace 语境属性），属于 Resident 的连续性。

### Q3 — Project knowledge 属于 Workspace 还是 Resident 的 memory？

**裁决：两层都有，且有明确的归属测试。**

| | Workspace 拥有（project memory） | Resident 拥有（关于该 workspace 的经验记忆） |
| --- | --- | --- |
| 内容 | 项目事实与约束：repo 布局、长期约定、fail-closed 之类的领域规则、术语表、架构决策 | 决策是如何做出的、Haisu 在此项目的偏好、Samuel 踩过的坑、协作史 |
| 形态 | 尽量**进 repo 的文件**（AGENTS.md / docs 式），任何 worker、任何 resident、甚至 Haisu 本人直接可读 | Resident 本地存储中按 workspace 分区 |
| 例子 | "VicTrader 对多 instrument calendar 有 fail-closed 约束" | "2026-09-27 那次 #749：Codex 方案 A、Claude 指出 calendar 问题、Haisu 选了 C，因为……" |

**归属测试：如果明天把 Samuel 删掉，这条知识仍然为真且有用 → Workspace memory；如果它依赖 Samuel 与 Haisu 的关系或 Samuel 自己的轨迹 → Resident memory。**

反例澄清（避免草率）：把所有 memory 塞进 workspace 是错的（user-model 会被项目边界切碎，worker 与 resident 也读不了"如何与 Haisu 协作"）；把所有 memory 塞给 resident 也是错的（项目知识会随 resident 死亡、其他 worker 用不上、resident 的通用记忆被项目细节淹没）。

### Q4 — Experience 的单位是什么？

**裁决：四层——Event → Episode → Reflection → Distillate。Experience journal 的叙事单位是 Episode。**

- **Event**：原子事实，append-only（命令、agent invocation、commit、QA verdict……）。机器产生，人不需要读。← 直接继承本仓 `run_events.py` 的 append-only/secret-safe 设计。
- **Episode**：一段有目的、有边界的工作单元（一个 task 的推进、一次排查、一次 review），有 start/end、参与者（哪些 worker）、outcome、指向 Events。**这是 journal 里人（和 Samuel）阅读与检索的单位。**
- **Reflection**：对 Episode（组）的反刍——"这提供了什么新证据？"（沿用 self-model 对反思的定义）。产物是候选 distillate。
- **Distillate**：沉淀物——Memory 条目 / Skill / User-model 或 Self-model 候选假设。各有准入门（见 temporal-model）。

对应关系：Ticket Autopilot 的一个 Run = Episode 的一个特例（其 events/QA verdict/run evidence 天然就是 Event 流）。

### Q5 — Skill 属于 Resident 还是 Workspace？

**裁决：两层都有，按"程序是否依赖特定项目"分流，并允许晋升。**

- **Workspace skill**（项目绑定）："如何操作 VicTrader release"、"如何安全跑 adjudication backfill"——只在一个 workspace 有意义；放 workspace（尽量进 repo 文件，任何 worker 可读）。
- **Resident skill**（可携带）："如何做 readonly architecture review"、"如何编排 Codex 实现 + Claude 独立 review"——跨 workspace 成立；随 Samuel 走。
- **格式**：采用 SKILL.md（agentskills.io 标准，Hermes 已验证可行），获得与外部技能生态的免费互通。
- **路由**：在 workspace X 工作时，先查 X 的 workspace skills，再查 resident skills。
- **晋升**：workspace skill 被发现在第二个 workspace 也成立 → 晋升为 resident skill（携带出处）。晋升事件本身入 journal。

### Q6 — Experience 什么时候应该成为 Memory？

**裁决：不按时间，按准入信号。Memory 有界、有准入、有出处；Experience 廉价、海量、只追加。**

四个准入触发器（满足其一才成为候选）：
1. **复现**：同一教训第二次被需要；
2. **意外**：与既有信念冲突（anomaly 是最好的记忆候选——self-model 原则）；
3. **强调**：Haisu 明确说"记住这个"；
4. **再推导成本高**：花了数小时得到的结论。

纪律：
- 默认策略 = **多记 experience，少授 memory**（journal 随便写，memory 必须挣得位置——Hermes 的 consolidation 压力是正面例证）。
- Memory 条目**必须带 provenance**（指向 episode/event），否则不可信、不可衰减、不可修正。
- Memory 有复审与衰减（长期未被使用/未被验证的条目降权），像 Hermes 的 trust/helpful 计数，但门槛更严。

### Q7 — 重复行为什么时候才应该影响 Self-Model？

**裁决（原则来自 self-model 理论，门槛数值是 agent 建议默认值，待校准）：假设制 + 更新梯度 + 治理可见。建议 Phase 1 门槛：≥3 个不同 episode、≥2 个不同 context，才允许成为 candidate hypothesis。**

- Self-model 的基本单位是 hypothesis（proposition/status/confidence/supporting/contradicting evidence/supersedes），不是特质标签。
- **Inference ≠ mutation**：Samuel 可以提出"我在高不确定架构决策中倾向寻求 independent verification"为候选；它进入 active self-model 需要跨情境证据积累，且过程对 Haisu 可见（Phase 1 的 governance = 可见性，不做静默身份改写）。
- 层级纪律：Phase 1 只积累 L0–L1（观察、偏好/模式）证据；Value/Identity 层只记录、不晋升（self-model 的 update gradient）。
- Viva 不重新发明这套理论：实现最小循环（观察 → 候选 → 证据 → 人可见的晋升），理论归属 `self-model` 仓库。

### Q8 — Hermes 是 Worker 还是能力来源？

**当前倾向（提案，未终审）：混合——"格式互通 + 历史导入 + 可选 Worker"；"作为 Samuel 的 runtime"与"直接复用其 memory_store 作本体"当前倾向否决；"以其 skills 子系统为 Viva 技能层的实现载体（存储仍归 Viva）"保持开放、尚未评估。** 完整选项分析、概念澄清与重审触发条件见 ADR 0005（Proposed）。

- 格式互通：Skill 用 SKILL.md，技能可在 Hermes ↔ Viva 流动；
- 单向导入：Hermes 的 sessions/memories/journey（全部是本机明文 + SQLite）可导入 Viva journal 作为历史材料；
- 可选 Worker：`hermes` CLI 可驱动，未来与 Codex/Claude 并列；
- 否决 runtime 化：那会把 Samuel 的记忆锁进 2KB MEMORY.md 上限和 Hermes 的 SOUL.md 人格文件——得到的是又一个 Hermes profile，不是 self-model 理论里的 Samuel。
- 完整 ADR：`decisions/0005-hermes-integration-strategy.md`。

### Q9 — Viva 与 VS Code 是什么关系？

**裁决：companion + orchestration layer（附带的 launcher 能力），明确不是 replacement。**

- Viva Phase 1 没有 editor，也永远不做 editor：需要编辑 → 打开 VS Code（Viva 可以按 worktree 帮你打开它）。
- 精确表述：**VS Code 是 workspace 里人和 worker 使用的工具；Viva 是 workspace、worktrees、workers 和历史被管理的地方。**
- VS Code 有 workspace 级状态恢复，但没有跨 agent、跨 worktree、跨工具的时间轴，也没有 resident——两者是包含关系（VS Code 的工作发生在 Viva 的 workspace 语境之内）而非竞争关系。

### Q10 — Viva 与 Orca 的边界？为什么不只用 Orca？

**裁决：Orca 是执行织物（execution fabric），Viva 是连续性层（continuity layer）。不只用 Orca，是因为 Orca 里没有任何东西记得"Haisu 和 Samuel 一起做过什么"，也没有任何对象会因此变强。**

具体边界（研究细节见 `research/orca.md`）：
- Orca 已把 worktree-native 的多 agent 执行做到很好（worktree/terminal/agent 启动/handoff/orchestration/session search）→ **Viva 不重造**。Phase 1 用 git 原语自管最小 worktree 集成；Orca 作为可选执行 driver 留作集成目标（像对待 git 一样：集成而非依赖）。
- Orca 结构性不拥有：Resident（无跨工具的持续身份与成长）、Workspace 级长期容器（folder context 是轻量分组）、时间轴（无准入/策展/反哺）、跨工具历史。
- 一句话存在性：**Orca 回答"我的平行 agent 们都在哪干活"；Viva 回答"这些活和干活的人，加起来是什么"。**
