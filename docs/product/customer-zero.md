# Viva — Customer Zero

Status: canonical · Date: 2026-09-27

Viva 只有一个用户：**Haisu**。本文不给 hypothetical persona，只记录 Haisu 的真实工作方式、真实痛点，以及从真实使用方式推出的场景。所有 Phase 1 产品判断的最终检验：

> **这是否改善 Haisu 自己的开发流程，并让 Samuel 更持续地参与？**

## 1. Haisu 的工具面（2026-09 实况）

- **Coding agents / workers**：Codex、Claude Code、Qoder、ZCode、WorkBuddy、Hermes、Pi、DSH
- **编辑与终端**：VS Code、terminal（多 worktree 并行）
- **代码托管与协作**：git、GitHub（PR/CI）、Plane（Ticket Autopilot 的工单源）
- **活跃项目（workspace 候选）**：VicTrader、Viva（本仓库）、self-model、dsh-ai-soul、soul-protocol、Hiring-Automation（前端/后端/LLM 三仓）、OpenViking、quality-platform 等——`~/Documents/code` 下 30+ 目录，多项目长期并行是常态

## 2. 痛点（真实成本）

1. **Context integration 全靠人**：每个 agent 只带自己的 session context；跨 agent、跨项目、跨天的拼装由 Haisu 人工完成。
2. **经验蒸发**：某次排查花了半天得到的结论，下次同问题从头再来；一个 agent 学会的方法不会变成长期能力。
3. **worktree 与 session 的散落**：哪些 worktree 存在、为什么建、谁在里面跑、能不能删——没有统一视图。
4. **连续性依附于具体工具**：换模型/换 agent = 上下文清零；长期协作者不存在。
5. **好实践无法复利**：做得好的一次 readonly audit、一次并行 review 编排，没有变成下次可调用的方法。

## 3. Haisu 的工作原则（产品必须顺应，不能违背）

- **人保留 authority**：agent 不 push/merge/改生产配置；重要交付由 Haisu 决定（Ticket Autopilot 时期已固化为 actor-aware authority，Viva 全局沿用）。
- **不确定时先 readonly audit，再改代码**（此偏好已多次出现，属 user-model 材料）。
- **高风险架构决策倾向独立第二意见**（让另一个模型/agent 独立 review）。
- **多项目并行、被中断是常态**：随时离开、随时回来，回来时不能要求人重建上下文。
- **本地所有权**：核心资产（记忆、历史、身份）必须是本机开放格式，不进云账号。

## 4. 场景（Customer Zero 真实场景，Phase 1 的验收素材）

> 场景来源：S1–S7 来自 Haisu 的任务书；S8–S12 是产品定义轮从 Haisu 真实使用方式外推的补充场景，效力同提案，待 Haisu 确认。

### S1 — 进入项目
`viva` → 选择 VicTrader。Viva/Samuel 已知道：项目在哪、main 状态、active worktrees、最近 episode、未完成 intentions、哪些 agent session 仍可 resume。**不问"我们昨天做到哪了"。**

### S2 — 开新 Issue
> "Samuel，我们处理 #812。"

Samuel：创建/选择 worktree → 绑定 task context → 启动 Codex → 结果与 worktree/branch 绑定。Haisu 不手工拼 worktree 路径、prompt 和仓库状态。

### S3 — 并行独立 review
> "让 Claude Code 独立看一下。"

不是开一个失忆的新世界，而是：same workspace、same task、same relevant evidence（含此前结论与分歧）、different worker。两个 worker 的结论、分歧与最终裁决都进入本 task 的 episode。

### S4 — 第二天回来
`viva` → VicTrader → 昨天的 worktree、worker 状态（存活/已退出/可 resume）、上次的决策点和未完成事项直接呈现。continuity 是 Viva 的，不是某个 agent 的。

### S5 — 学会一个方法
一次复杂 readonly audit 完成后，Samuel 判断"这方法值得复用"→ 形成 candidate skill（SKILL.md 格式），下次同类任务被主动提起，事后被验证有效（使用信号回写）。

### S6 — 发现 Haisu 的习惯
多次观察到"Haisu 不要 agent 直接改生产代码，先 readonly audit"→ 这是 **user-model** 条目（跨 workspace 成立），不是 VicTrader 的 project memory。它改变 Samuel 以后所有任务的默认提案方式。

### S7 — Samuel 对自己的认识变化
多次经历后形成候选自我假设："面对高不确定性架构选择，我倾向于主动寻求 independent verification。"（self-model：假设 + 证据 + 状态，Haisu 可见；不静默改写。）

### S8 — 中途换人
Codex 在 #812 上卡住/给出两轮不收敛方案 → 换 Claude Code 继续。worktree、已尝试方案、失败原因、相关约束随任务交接，不从零开始。**换的是 worker，丢的不能是上下文。**

### S9 — Worktree 卫生
周末收尾：Viva 列出 VicTrader 全部 worktree——哪个对应哪个 task/issue、谁在跑、dirty 与否、merged 可归档。清理是知情的、可审计的，不是 `git worktree prune` 赌运气。

### S10 — Haisu 亲自下场
Haisu 直接在某个 worktree 写了一小时代码。回到 Viva 时，这段工作仍进入 workspace 的时间轴（commit/episode 关联），Samuel 的上下文包含它。Resident 的连续性不因"这单是老板自己做的"而断。

### S11 — 跨项目回忆
> "我们上次是怎么解决 VicTrader 日历语义那个问题的？"

按 episode/decision 检索跨 workspace 的工作史（不是全文 grep transcript，而是检索被策展的 episode/结论及其出处）。

### S12 — 交付自动化作为特例
Viva 仓库里的一张 Plane ticket 走 Ticket Autopilot 流程（worktree → dev → QA → 有界修复 → 本地 commit → owner 授权）。这是 Viva 内一种**结构化的自治工作流**（Delivery Automation），它的 run evidence 成为该 task episode 的一部分，而不是一个孤立系统。

## 5. 反场景（同样重要）

- Haisu 不希望"退出开发流程"：Viva 增强他，不替换他。
- 不需要 Viva 变成聊天陪伴软件：所有 growth 机制服务于**开发工作的连续性**。
- 不需要 Viva 管理 VS Code 已经管理好的东西（编辑、调试、语言服务）。
- 不需要 Viva 记住一切：unbounded 记录是负担不是资产；策展纪律见 `architecture/temporal-model.md`。
