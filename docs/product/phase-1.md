# Viva — Phase 1 Scope

Status: **Proposed**（范围与验收标准待 Haisu 批准）· Date: 2026-09-27 · 上游：`vision.md` / `workflows.md` · 纪律：本文定义范围，不实现

## 0. 已建成的地基（2026-09-27 现状，先行实现轮产出）

一轮先行实现已落地 Phase 1 的**外壳**（见 `docs/architecture/viva-transition.md`；本轮文档治理未改任何代码）：

- `viva` CLI + Textual TUI（resident/workspace/worktree/worker/runtime 状态与 live journal）；
- Resident registry、Workspace registry（本地 git repo 注册、跨 session 持久化）、Worktree 只读发现（基于交付子系统的 worktree 服务）、Worker registry + 可用性探测 + 受控 run；
- Experience journal（append-only、secret-redacted，`~/.viva/experiences/journal.jsonl`）；
- 权限词汇表（READ / PROPOSE / ACT_WITH_APPROVAL / ACT_AUTONOMOUSLY / FORBIDDEN；Phase 1 无自治调用）；
- 诚实约束：journal 是 experience 不是 memory；memory/self-model 不存在且不得伪装（AGENTS.md Honesty Constraints）。

**尚未建成**（= 本文其余部分定义的剩余范围）：workspace 级 project memory 与上下文注入、Task 对象、Episode 叙事层与检索、worker session 的 who/where/what/since-when 追踪、memory/skill 的手动策展流、user/self-model 假设文件、以及 north-star loop 的端到端成立。

## 1. Phase 1 的一句话目标

> **让"进入 workspace → 续上上下文 → 开 worktree → 派 worker → 工作 → 换人 → 留下记录 → 改天回来继续"这个闭环，在 Haisu 的真实项目上成立。**

不做完整 Jarvis。Phase 1 的价值判据：`workflows.md` 的 north-star loop 在至少一个真实 workspace（建议：本仓库 Viva 自己）上跑通，S1/S4/S8 可真实发生。

## 2. Phase 1 必须解决（按对象给最低标准）

| 能力 | 最低标准（Phase 1） | 复用来源 |
| --- | --- | --- |
| **Resident continuity** | Samuel 的状态 = 本机开放格式文件（身份、memory 分区、skills、journal 索引、user/self-model 假设文件）；`viva` 启动即"续上"；换底层模型不换状态文件 | self-model 理论；Hermes 反例（不采用 SOUL.md/2KB 上限） |
| **Workspace management** | Workspace 注册表（名字、repositories、project memory 指针、状态）；进入/切换/最近列表；project memory 尽量以 repo 内文件为准 | VS Code 模型；本仓 WorkBuddy MEMORY.md 的教训（全局一份混装 → 必须分区） |
| **Worktree management** | 创建/列出/归属/状态（active/archived/dirty/merged）/知情的清理；受保护分支不可动 | 本仓 `git_worktree.py`（protected-branch guard、disposable worktree）；Orca 的状态语义 |
| **Worker management** | Worker catalog（CLI 可用性探测）+ 以受控 permissions 启动 + 追踪 who/where/what/since-when | 本仓 `agent_catalog.py`（probe-backed、保守 token 白名单） |
| **Session management** | WorkerSession 与 ResidentSession 分开登记；存活/退出/可 resume；transcript 位置可寻 | 本仓 `agent_sessions.py`（codex session-id 发现） |
| **Experience journal** | append-only Event + 人可读 Episode（task、参与者、outcome、指向 events）；secret-safe | 本仓 `run_events.py`（redact、append-only） |
| **Minimum memory** | 手动策展的最小 memory：workspace 分区 + resident 全局分区；条目必带 provenance；有界 | Hermes consolidation 纪律；temporal-model 准入门 |
| **Skill growth** | SKILL.md 格式；workspace skill（可进 repo）+ resident skill；手动收尾时捕捉（S5），使用可记录 | Hermes/SKILL.md 标准（agentskills.io） |
| **Self-model interface** | 候选假设文件（status/confidence/evidence/provenance）+ 反思记录；与 `self-model` 仓库理论对齐；**不实现理论、不自动改写** | self-model 仓库（hypothesis 草案、update gradient） |

## 3. Phase 1 明确不解决（Non-goals）

```text
multi-user · team collaboration · cloud SaaS · mobile · voice
WeChat · QQ · email · calendar · full macOS control · GUI-first
avatar · marketplace · generic agent creation · multi-resident UX
enterprise permissions · billing · social features · distributed agents
```

以及（本阶段的工程性 non-goals）：

- **不实现** editor / file tree / IDE 功能（VS Code 承担）；
- **不重造** Orca 的 terminal 编排 / handoff / orchestration（集成可选，依赖禁止）；
- **不采用** Hermes runtime 或其 memory schema 作为 Samuel 本体（ADR 0005）；
- **不做** 自动 memory 策展/自动反思调度（W7 允许手动触发）；
- **不做** 通用框架、plugin 系统、为"以后可能需要"的抽象；
- **不做** `src/ticket_autopilot/` 的 package rename / 大规模重构 / 数据库 schema（增量迁移规则见 AGENTS.md 与 `viva-transition.md`；已落地的 `src/viva/` 分包与 Textual 选型是先行轮的既成决策，本文不翻案）；
- **不做** 交付自动化行为的扩展（Ticket Autopilot 冻结在现有边界，ADR 0004）。

## 4. Surface

`viva` → 类 Claude Code TUI（交互纪律见 `workflows.md` §3）——**已建成初版**（Textual；决策记录在 `viva-transition.md` §5），剩余范围是围绕 Resident/Workspace/Worktrees/Workers/Task/Activity 的信息架构深化。Phase 1 的唯一一等 surface 是 TUI；Ticket Autopilot 的 localhost Web 控制器保留为其子系统的操作面，不再是 Viva 的前门。

## 5. 成功标准（可验收）

1. **闭环**：north-star loop 在 ≥1 个真实 workspace 全程走通 ≥3 个工作日，期间 Haisu 未发生"人工重建上下文"。
2. **连续性**：S4 场景测试——隔天回来，未问"做到哪了"，Samuel 的开场即包含上次的状态与未竟事项。
3. **换人**：S8 场景测试——同 task 换 worker，新 worker 首轮即获得任务级上下文包。
4. **复利**：≥1 个 skill 或 memory 条目由真实 episode 产生，且在后续 episode 中被引用 ≥1 次。
5. **边界**：全部数据为本机开放格式文件；卸载 Viva 后，workspace 的 project memory 与 Samuel 的状态文件仍可人肉阅读。

## 6. Phase 1 之后的自然顺序（仅记录，不承诺）

- Delivery Automation 作为 Task 的一种结构化 workflow 挂载进 workspace 语境（W8 深化）；
- Orca 作为执行 driver 的集成评估；
- 反思循环的自动化（在准入门被人工验证可靠之后）；
- 跨 workspace 检索（S11）的深化。
