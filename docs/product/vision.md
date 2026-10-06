# Viva — Vision

Status: canonical · 版本：2026-09-27 · Owner: Haisu · 本文取代本文件 2026-09-27 早先的"单 Resident 开发环境"定义（原因见 §1.1）

---

## 1. 一句话定义

> **Viva 是 Haisu 的本地优先个人 AI 协作系统：多个持续存在的 AI 成员分工协作，保留各自的历史与知识，并调用真实工具完成工作。**

英文：*Viva is Haisu's local-first personal AI collaboration system: several persistent AI members keep their own history and knowledge, collaborate, and drive real tools to get work done.*

### 1.1 这次定义改了什么，为什么

| 旧定义（已作废） | 现在 | 原因 |
| --- | --- | --- |
| "Haisu 与**一个**常驻 Agent Samuel 协作的本地开发环境" | 多个持续存在的 AI 成员 | Haisu 的真实工作方式是让职责不同的成员长期协作（PM/调度、开发、QA、运维、研究），同一成员同时参与多个任务 |
| 把 multi-resident UX 与 AI 团队协作排除在范围外 | 是产品本体 | "只有一个人类用户"曾被错误地推出"没有 AI 团队"；客户数量与成员数量是两件事（ADR 0006） |
| Workspace 取代所有 Project 概念 | `Workspace ≠ Project ≠ Repository ≠ Worktree ≠ Task` | 需要表达"一个 Viva 实例管理多个项目、一个项目多个仓库"（ADR 0009） |
| 禁止所有 Worker 发起的委派 | 协调成员可在用户授予的范围内调用其他成员 | 否则"协调"只是文案（ADR 0007） |
| Ticket Autopilot 作为保留子系统 | 已退役，只复用仍有用的三个能力 | 它的对象（ticket/run/verdict）与 Viva 的对象（Task/Execution/Grant）不同源（ADR 0008） |

**没有改变的部分**：单人类用户、本地优先、连续性优先、诚实约束（不假装记得、不假装成长）、`Viva ≠ Samuel`（名字与职责是配置数据）。

## 2. 为什么存在

Haisu 每天使用大量开发工具与 AI agents。痛点不是"缺一个更聪明的 agent"，而是：

- 每个 agent 只带自己的会话上下文，跨成员、跨项目、跨天的拼装由 Haisu 人工完成；
- 经验蒸发：一次排查半天得到的结论，下次从头再来；
- 成员不存在：换模型/换工具等于换人，长期协作无从谈起；
- 好实践无法复利：一次漂亮的并行 review 编排不会变成下次可调用的方法；
- 任务历史依附于某个进程：worker 一死，做到哪、为什么失败、还欠什么，全部丢失。

Viva 的存在就是把这些变成**Viva 持续保留的资产**：成员持续存在，任务持续存在，执行的归属与结果持续存在。

## 3. 给谁用：Customer Zero

**Viva 只服务一个人类用户：Haisu。** 他拥有多个 AI 成员，每个成员有名字、职责、模型与工具绑定。成员名字与职责是**配置数据**，不是产品内置人格。

不解决：多人类用户、团队账号、企业权限、marketplace、plugin 生态、云服务、通用 onboarding、商业化。

产品判断标准（所有本轮决策回溯到这里）：

> **它是否让 Haisu 的工作由多个长期成员持续推进，并让这段协作的上下文、历史与能力积累下来？**

**首版投入使用标准（2026-09-28）**：Viva 必须能从普通终端独立启动，并替代 Orca 的多 worktree 并行开发入口。成员与任务的持续性语义建立在自己可用的执行体验上；Orca 不是必需宿主。具体验收见 [first-usable-version.md](first-usable-version.md)，当前尚未达标。

## 4. 存在性测试（为什么不是"几个 CLI 加一个终端"）

| 工具 | 它记得什么 | 它不记得什么 |
| --- | --- | --- |
| VS Code | workspace 的 UI 状态 | 跨工具历史；成员；任务 |
| Orca | worktree 状态与 transcript | 成员的长期身份；任务对象；知识归属 |
| Codex / Claude Code / Qoder | 当前会话的上下文 | 其他成员；上次派发；失败原因；任务未完成项 |
| Ticket 系统（Plane/Linear） | 工单状态 | 谁在什么时候用什么模型做了什么、结果如何 |

**Viva 不可替代的中心**：它管理**多个成员 × 多个任务 × 多次真实执行**的关系与历史，并让这些历史反哺下一次工作。

两条使这个中心成立的约束：

1. **记得 ≠ 存档**。只是"集中记录"就只是日志仓库。Viva 的记录必须能被下一次工作取用：任务的交接简报、知识条目的使用记录、拒绝的原因、恢复后的未完成项。
2. **本地所有权**。成员的状态、历史与知识是 Haisu 机器上的开放格式文件，不锁进任何供应商账号。

## 5. 不是什么

- **不是全自动开发**：人是决策者，成员是执行与协作方；重要动作仍需授权。
- **不是 IDE**：不做 editor / file tree / debugger。
- **不是通用 agent 平台**：不服务"让所有人创建自己的 AI 团队"。
- **不是记忆/成长的模拟器**：没有实现的成长能力不得出现在 UI 与文案里。
- **不是 Ticket Autopilot 的延续**：旧子系统已退役（ADR 0008）。

## 6. 产品模型概览

```text
┌────────────────────────────────────────────────────────────────────────┐
│ Members（Resident）                                                    │
│   identity · role · history · knowledge · self-model candidates（未实现）│
├────────────────────────────────────────────────────────────────────────┤
│ Bindings（可替换）                                                     │
│   Role（职责，配置数据） · Engine（模型绑定） · Worker（执行工具 CLI）     │
├────────────────────────────────────────────────────────────────────────┤
│ Work                                                                  │
│   Workspace → Project → Repository → Worktree                          │
│   Task（意图） ──► Execution（一次真实执行：成员×任务×模型×工具×位置×授权）│
│                    ▲ Grant（来源 / 范围 / 委派关系）                    │
├────────────────────────────────────────────────────────────────────────┤
│ Knowledge（四类归属）                                                  │
│   personal · project · team · skill（+ 复用证据）                       │
└────────────────────────────────────────────────────────────────────────┘
```

核心纪律：**成员不是进程，任务不是 worktree，执行必须有归属与授权。**

## 7. 现状（2026-09-28 更新）

> 历史注记：本节的"已实现"最初以 Python + Textual 实现并在 `tests/viva/`
> 有证据；V13（issue #22）退役了 Python 运行时。下述能力现由 Rust 实现
> 承载，证据在 `crates/viva/tests/`（逐 issue 验收测试）。V13 退役映射见
> `docs/validation/v13-python-retirement.md`。个别条目以 Rust 侧实际交付
> 为准（经验/日记能力首版未建，无假声明）。

已实现并有测试证据：

- 多成员 + 角色/模型/工具配置，换模型保留历史与知识，不可用时报错不替换；
- Workspace / Project / Repository / Worktree / Task 对象与 `task brief` 交接简报；
- 执行注册表：并发、隔离 worktree、归属固定、停止互不影响、重启后如实恢复；
- 委派授权：grant 的来源/范围/父子关系，越权拒绝并留原因；协调成员真实调用 `viva …`；
- GitHub 只读关联（issue/PR/checks/review 证据）；
- 知识四类归属 + 复用证据；experience journal（append-only、脱敏）。

**尚未实现（不假装）**：自动反思循环、Self-Model 演化、User-Model 演化、Jev 决策辅助集成、daemon、远端写操作自动化、跨设备同步。见 `phase-1.md` §4。

## 8. 文档地图

| 问题 | 文档 |
| --- | --- |
| Viva 是什么、为什么、给谁 | 本文 |
| 核心对象与关系 | `product/product-model.md`、`architecture/domain-model.md` |
| Haisu 的真实场景 | `product/customer-zero.md` |
| 办公流与验收线 | `product/workflows.md` |
| 本轮范围与未实现项 | `product/phase-1.md` |
| 最新代码基线与后续路线提案（非验收 PASS） | [roadmap.md](roadmap.md) |
| 概念架构 / 时间轴 | `architecture/conceptual-architecture.md`、`architecture/temporal-model.md` |
| 关键决策（含被取代的） | `decisions/0001`–`0011` |
| Rust/Pi 与 SQLite 技术裁决（目标与现状） | [ADR 0011](../decisions/0011-rust-host-and-tui.md) |
| 迁移与退役记录 | `architecture/viva-transition.md` |
| 灵感来源研究 | `research/` |
