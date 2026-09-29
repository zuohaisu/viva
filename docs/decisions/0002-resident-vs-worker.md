# ADR 0002 — Resident 与 Worker 严格区分

Status: **Superseded by ADR 0006**（2026-09-27；保留作历史出处）· Date: 2026-09-27 · 上游：`docs/product/product-model.md` Part II

> **取代记录（2026-09-27，ADR 0006）**：本文的**核心区分（Resident 长期存在、Worker 可替换、成长只附着 Resident 侧、worker 无交付权）继续有效**，但框架有三处不足，由 ADR 0006 取代：
> 1. 只有单一 Resident（Samuel），无法表达多成员协作；
> 2. 缺少 **Role** 与 **Engine** 两层（本文把"认知模型"直接当 substrate 讨论，却没有把它作为可配置绑定对象）；
> 3. 用"WorkerSession"承担执行记录，无法区分"一次真实执行的归属"与"工具进程"，也无法表达同一成员并行的多个执行。
>
> ADR 0006 的新关系：`Resident ≠ Role ≠ Cognitive Engine ≠ Worker ≠ Execution Session`。

> 效力说明：**"Resident 与 Worker 必须严格区分、真正持续积累历史的是 Samuel"是 Haisu 任务书中的显式要求**，不是本文的发明。本文把该要求展开为具体的关系模型（WorkerSession 双锚、两种 Session 分开、成长只附着 Resident 侧）——**展开的细节是产品定义轮的提案**，其中的具体门槛与映射未经 Haisu 逐项接受。

## Context

Viva 的词汇里同时存在"长期协作者"（Samuel）与"可替换工具"（Codex / Claude Code / Qoder / Pi / Hermes）。若不严格区分，会出现两类事故：把工具的能力误当成长期身份的成长（工具一换，"人格"清零）；或把 Resident 做成某个具体模型的 prompt 人设（self-model 证伪测试：换 LLM = 换 Self，则解耦失败）。self-model 仓库的理论（Samuel ≠ GPT ≠ Prompt；LLM 是可替换认知 substrate）是本决策的理论基础。

## Decision

1. **Resident**（当前：Samuel）长期存在，拥有 history / experience / memory / skills / self-model / user-model / relationship；其状态是可迁移的开放格式数据，不绑定任何模型、session 或 worker。
2. **Worker** 是可启动、可停止、可更换、可升级、可失败、可并行的执行工具；WorkerSession 隶属 Worktree（空间锚）、服务 Task（意图锚），**不属于 Resident**。
3. **Cognitive Engine** 是某一时刻提供认知的模型（GPT / Claude / GLM…），是 Resident 的 substrate，不是 Resident 本体。
4. 目标分工："Samuel asks Codex to implement; Samuel asks Claude to review; Samuel compares and remembers." 真正积累历史的是 Samuel。
5. 权限继承：Worker（与 Resident 发起的自动化）永不获得 repository-owner 交付权（actor-aware authority，AGENTS.md）。

## Consequences

- 换模型/换 worker 不得清零任务上下文（task 上下文与 worker 解耦，场景 S8）。
- 成长（memory/skill/self-model）只发生在 Resident 侧；对 worker 的改进只体现为 catalog/invocation 配置。
- Resident 的连续性可被检验：更换底层模型后，其状态与自我假设文件必须原样有效（self-model 的 Model-Switch Continuity 协议是验收参照）。
