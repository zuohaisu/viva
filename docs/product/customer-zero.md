# Viva — Customer Zero

Status: canonical · 版本：2026-09-27（AI Office 修订）· 本文不给 hypothetical persona，只记录 Haisu 的真实工作方式

Viva 只有一个人类用户：**Haisu**。他不是一个"使用工具的人"，他是一间 AI 办公室的主人：多个 AI 成员长期存在，各有职责，同时推进多个任务。所有产品判断的最终检验：

> **这是否让 Haisu 的工作由多个长期成员持续推进，并让这段协作的上下文、历史与能力积累下来？**

## 1. Haisu 的办公室（2026-09 实况）

- **AI 成员**：Samuel（PM/调度）、Oliver（运维）、Alice（QA/review）、Deven（开发）、Richard（研究）——名字与职责是 Haisu 配置的数据；成员可以增删改角色。
- **认知模型**：Claude、GPT、GLM、DeepSeek、本地模型等，按成员绑定，随时可换。
- **执行工具（CLI）**：Claude Code、Qoder CLI、Codex、ZCode 等。
- **工作方式**：多项目并行、随时被打断、经常换人（同一任务换成员继续）、经常让两个成员独立看同一件事。
- **代码与协作**：git、GitHub（issue/PR/CI）。外部业务连接本阶段只考虑 GitHub；Jev 是可选的决策辅助，**不是系统成立的前提**。

## 2. 痛点（真实成本）

1. **协调靠人**：谁该做什么、做到哪、为什么停——没有一个地方记录，全在 Haisu 脑子里。
2. **历史依附于进程**：worker 一死，任务上下文蒸发；换人等于从零开始。
3. **经验蒸发**：一次排查半天的结论，下次重来。
4. **成员不存在**：换模型/换工具就是换人；成员之间无法委托。
5. **并行难管**：几个 agent 同时改代码，谁在哪个 checkout 里做什么不透明。
6. **越权无痕**：agent 做的动作背后是谁授权的，事后说不清。

## 3. Haisu 的工作原则（产品必须顺应）

- **人保留 authority**：成员不 push 保护分支、不 merge、不自授权；远端动作要明确授权并留审计。
- **先只读审，再动代码**（高风险场景尤其如此）。
- **高风险决策要第二意见**：让另一个成员独立 review。
- **打断是常态**：随时离开、随时回来，回来时不能要求人重建上下文。
- **本地所有权**：成员状态、任务历史、知识必须是本机开放格式。

## 4. 场景（本轮验收素材）

> S1–S9 与 `workflows.md` 的验收线一一对应；括号内是本轮的实现与证据位置。

**S1 — 派发两个任务给同一成员**
> "Samuel，把 #150 和 #151 交给 Deven。"

Samuel 创建/分配两个 Task 给 Deven；两个执行各在自己的 worktree 里真实并行推进（`office dispatch` × 2；`tests/viva/test_acceptance.py::test_scenario_1_and_2…`）。

**S2 — 执行期间换人/换语境**
Haisu 在 Deven 跑着的时候去看 Alice、切到另一个 workspace。回来时事件归属仍然正确：那次执行仍是"Deven 在 #150 上做的"（归属固定在执行记录里；`test_scenario_4…`）。

**S3 — 独立只读 review**
> "让 Alice 独立看一下。"

Alice 以 `read_only` 模式进入同一任务的产出（同一个 worktree），独立给结论；她的执行与 Deven 的执行在任务上并列可查（`test_scenario_3…`）。

**S4 — 只停一个任务**
> "停掉 #150，别动 #151。"

被停的执行进入 `stopped`，另一个仍在跑（`test_scenario_5…`）。

**S5 — 中断与恢复**
机器重启。Viva 如实报告：谁还在跑、谁已经退出（结果不明）、哪些可以恢复；**不会重跑已经完成的执行**；任务的目标、约束、已有产出、已试方案、失败原因都能取回（`office recover` / `task brief`；`test_scenario_6…`）。

**S6 — 换模型不清历史**
Deven 从 Claude 换到另一个模型。记录、历史事件、知识条目全在；新执行记录新的 model（`resident engine`；`test_scenario_7…`）。

**S7 — 越权被拒绝且留痕**
一个成员试图派发到它没有授权的任务（或试图用更宽的模式）。拒绝理由进入 grant ledger 与 journal，能被查（`test_scenario_8…`；`viva office grant`）。

**S8 — 换人接手**
Deven 卡住/不收敛。`viva task brief <id>` 给出目标、约束、工作位置、已试方案与失败原因、已有产出、未完成项——换 Oliver 或 Alice 接着做。

**S9 — 关联到 GitHub**
Task 关联到 repo + issue；执行在分支上产出；PR 出现后能读到 checks 与 review 证据，并追回正确的 task 与产出（`github link` / `github evidence`；`test_scenario_9…`）。

## 5. 反场景（同样重要）

- 不需要 Viva 变成聊天陪伴软件：所有机制服务于**工作**。
- 不需要 Viva 管理 VS Code 已经管理好的东西（编辑、调试、语言服务）。
- 不需要 Viva 记住一切：不加以整理的记录是负担，策展纪律见 `architecture/temporal-model.md` §5。
- 不需要"全自动开发"：Haisu 保留决策、review、方向与授权。
- 不需要多人类用户协作：多**成员**不等于多**用户**。
