# ADR 0001 — Viva 产品边界：Customer Zero 单用户、本地优先、连续性层

Status: Accepted（部分被取代）· Date: 2026-09-27 · Decider: Haisu（owner 显式指令）· Supersedes: IDEA.md（2026-07 版）中 "ticket-driven delivery 为 North Star" 的边界 · **Superseded by: ADR 0006**（撤回"单人类用户 ⇒ 没有 AI Team/Member"的推论，并把 multi-resident 纳入范围；本文的"单用户、本地优先、连续性层"三条仍然有效）

> **取代记录（2026-09-27，ADR 0006）**：Haisu 已明确 Viva 是"一个有多个 AI 成员的 Personal AI Office"。"只服务一个**人类**用户"仍然成立；由它推出的"因此没有 AI 团队/成员"作废，本文 §Consequences 中"拒绝 team"一条据此收窄为"拒绝多人类用户与企业级协作"，不拒绝 AI 成员之间的协作。

> 效力说明：单用户（Customer Zero）、本地优先、Ticket Autopilot 降级均为 Haisu 任务书中的**显式指令**，本文仅将其 codify；边界之下的具体设计（Phase 1 范围、门槛数值等）不在本 ADR 的接受范围内，见 `phase-1.md`（Proposed）。

## Context

仓库原 North Star 是 ticket-driven automated software delivery（AI-Operation 时期）。2026-09 起产品判断变化：问题不再是"缺一个更聪明的 coding agent"，而是"工具之间缺少长期连续的工作环境"。同期一轮先行实现已把 `src/viva/` 外壳落地（`docs/architecture/viva-transition.md`）。产品需要一条明确的边界，防止为 hypothetical users 过度设计（本仓库曾因此发生过一次目标漂移，见 logs/goal-drift.md 2026-07-22）。

## Decision

1. **Viva 只服务一个用户：Haisu（Customer Zero）**；当前唯一 Resident 是 Samuel。所有 Phase 1 产品判断回溯到："它是否让 Haisu 每天的开发更顺畅，并让 Samuel 更持续地参与？"
2. **本地优先**：所有 Viva 状态（resident/workspace/journal）是本机开放格式文件；云、多设备、多用户均非目标。
3. **连续性层**：Viva 的差异化中心是"一个长期 Resident 与 Haisu 穿越所有工具后的连续工作史"（存在性测试见 `docs/product/vision.md` §4）。
4. 为 hypothetical future users 增加复杂度的设计**默认不做**（IDEA.md 的 `Viva ≠ Samuel` 是代码诚实规则，不是产品扩张理由）。

## Consequences

- 拒绝：multi-user、team、enterprise、marketplace、generic onboarding、monetization、cloud、全 OS 兼容、"让人人创建 AI Soul"。
- 允许偶尔为"未来可能"付出的成本只剩一种：**数据格式保持开放可迁移**（这是 Customer Zero 自己今天的需要，不是别人的）。
- 若未来边界变化，必须走显式 reprioritization 记录（logs/goal-drift.md）。
