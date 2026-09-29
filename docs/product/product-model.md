# Viva — Product Model

Status: canonical（对象与关系）· 版本：2026-09-27（多成员协作修订）· 上游：`vision.md` · 下游：`architecture/domain-model.md`

本文回答：**Viva 由哪些对象组成、成员如何绑定模型与工具、任务如何分配、执行如何记录、协调如何工作、知识属于谁。** 效力说明：对象与关系来自 Haisu 的显式决定（ADR 0006–0010）；具体字段与实现细节是设计选择，可随实现演进。

---

## Part I — 成员及其绑定

```text
Resident（成员）
 ├─ identity         id / name / created_at / notes
 ├─ role      ────►  Role（配置数据：做什么、允许哪些工作模式）
 ├─ engine    ────►  Engine（配置数据：模型 + 服务它的工具）
 ├─ tools     ────►  Worker（配置数据：具体 CLI 与其可用性）
 ├─ history   ────►  experience journal（append-only，按成员归属）
 └─ knowledge ────►  knowledge entries（personal / self-model candidate）
```

四条规则：

1. **名字与职责都是配置**。`Samuel`/`Deven`/`Alice` 是记录；没有任何逻辑分支依赖成员名。角色目录（`roles.json`）与引擎目录（`engines.json`）用户可编辑。
2. **模型与工具是可替换器官**。换 engine 只改一个绑定字段，并追加一条 `member.engine_changed` 事件（记录 from/to）；成员记录、历史、知识全部保留。
3. **不可用就报错，不替换**。配置的模型/工具不可用时，派发失败并说明原因与它配置的是什么；不会静默换一个模型或工具。
4. **不宣称 Self 连续性**。成员记录只证明"配置、事件与知识被保留"。没有 Self-Model 演化机制，成员记录里也没有任何记忆/人格字段（有测试锁定字段集合）。

角色示例（种子，可改）：

| role | 用来做什么 | 允许模式 | 默认模式 |
| --- | --- | --- | --- |
| `coordinator` | 拥有计划，派发并观察其他成员 | read_only, write | read_only |
| `developer` | 在隔离 worktree 里实现与修复 | read_only, write | write |
| `reviewer` | 独立只读 review，永不写 | read_only | read_only |
| `operator` | 运维与环境工作 | read_only, write | read_only |
| `researcher` | 调查与报告，不改文件 | read_only | read_only |

## Part II — 任务与执行

| | **Task** | **Execution** |
| --- | --- | --- |
| 是什么 | 意图（"处理 #150"） | 一次真实执行 |
| 存活 | 跨执行、跨成员、跨重启 | 进程结束即终态 |
| 拥有 | intent、status、assignees[]、work_location、outputs[]、unfinished[] | 成员、任务、模型、工具、工作位置、授权、结果 |
| 失败语义 | 不失败，只是还没完成 | 有明确终态（completed/failed/stopped/exited/unknown） |

**一个任务可以分配给一个或多个成员；一个成员可以同时拥有多个执行。** Deven 同时做 #150 和 #151 时：成员身份相同，任务上下文隔离，可写工作位置隔离（各自的 worktree）。

执行固定记录七件事（启动时写入，之后不被界面改动）：

```text
member（谁） · task（为什么） · engine+model（用哪个大脑） · tool（用哪个 CLI）
work_location（在哪：worktree / 只读仓库，含 mode）  · request（谁要求的：user / worker）
authority（凭什么：grant 的 id、actions、mode_max、delegated_from）
```

## Part III — 工作位置

| 任务类型 | 工作位置 | 理由 |
| --- | --- | --- |
| delivery / implementation / fix / chore | **必须**有独立 worktree（`~/.viva/worktrees/<repository>/<task-id>`） | 两个任务不能写同一个 checkout |
| research / planning / review | 只读地跑在仓库里，不创建 worktree | 只读工作不需要隔离环境 |
| 无仓库的任务 | 没有工作位置 | 纯记录/对话型任务 |

reviewer 属于第二种：**review 同一个任务时仍然进入该任务的 worktree，但以 `read_only` 模式运行**——审查的是真实产出，而模式保证它不写。

## Part IV — 协调与授权

```text
Haisu ──grant（来源=user，范围=task+actions+mode）──► Samuel（coordinator）
                                                        │
                        viva office dispatch --task … --to Deven
                                                        │  （花掉 grant）
                                                        ▼
                                        Execution(Deven, task, write)
                                           request = {worker, Samuel's execution id}
```

规则：

- 成员发起的派发/停止**必须**持有效 grant；没有 grant 直接 FORBIDDEN，并记录原因。
- **子 grant 不能放大**：actions ⊆ 父、mode ≤ 父、task 相同。
- **请求不被改写**：worker 请求不能声称用户来源；用户请求不能花 grant。
- **不是固定流水线**：dispatch 的目标（成员、任务、模式）由调用方决定；代码里没有 dev→QA 的硬编码顺序。
- 远端动作（push 保护分支 / PR / merge / approve）不在可授予的 action 集合内；只有 owner 能授权。

## Part V — 知识与归属

| kind | owner | 谁能看到 | 归属测试 |
| --- | --- | --- | --- |
| `personal_memory` | 成员 | 该成员参与的任务 | 描述这个成员自己的经验/偏好 |
| `self_model_candidate` | 成员 | 同上 | 关于这个成员自己的假设（本轮只记录） |
| `project_knowledge` | Project | 该项目的任务 | 删掉所有成员仍为真 |
| `team_knowledge` | 团队 | 所有任务 | 跨成员协作方式 |
| `skill` | 团队（可绑项目） | 所有任务 | 可复用 + 可验证（SKILL.md） |

纪律：条目必须带 provenance；**只有被后续执行实际使用过（`used_in` 非空）才算复用证据**；撤回留原因；没有自动晋升，也没有自动衰减（未实现，明说）。

## Part VI — 与旧裁决的关系（修正记录）

| 旧裁决（已作废） | 现在 | 原因 |
| --- | --- | --- |
| Q1：Workspace 是导航第一层，Resident 不是导航目的地 | 成员是常驻底座，Workspace/Project 是语境；TUI 同时呈现两者 | 多成员协作时"我在哪工作"与"谁在工作"是两个正交维度 |
| Q2：WorkerSession 隶属 Worktree、服务 Task | Execution 固定七元归属（含成员、模型、工具、授权） | 需要回答"谁在什么时候用什么做了什么、凭什么" |
| Q3/Q5：知识两层分储（workspace/resident） | 四类归属：personal / project / team / skill | 需要 team 层，且要与 Project（≠Workspace）对齐 |
| Q4：Experience 单位是 Event→Episode→Reflection→Distillate | 保持不变（reflection/distillate 仍是未实现阶段） | 本轮不实现自动反思 |
| "没有 Team/Member 对象" | 成员（Resident）是核心对象 | ADR 0006 |

## 附：十个问题的当前答案

1. **UI 第一层是什么？** 成员与语境并列：成员面板常驻（谁在、谁在跑、跑到哪），Workspace/Project 决定语境。
2. **执行属于谁？** 属于**任务**（意图）与**工作位置**（副作用），并固定记录成员与授权；成员是观察者/协调者。
3. **项目知识属于谁？** Project（删除成员仍为真）；成员的经验与假设属于成员。
4. **经验的单位？** Event →（Episode 为阅读单位）→ 人工策展为 Knowledge 条目 → 使用记录成为复用证据。
5. **技能属于谁？** 默认团队（可绑项目）；SKILL.md 格式，可移植。
6. **经验何时成为记忆？** 人工策展 + 准入门（复现/意外/强调/高再推导成本），必须带 provenance。
7. **重复行为何时影响 Self-Model？** 本轮不实现；只记录 `self_model_candidate`。
8. **Hermes 是 Worker 还是能力来源？** 见 ADR 0005（未变，本轮不涉及）。
9. **与 VS Code 的关系？** companion：Viva 管成员/任务/执行/历史，VS Code 管编辑。
10. **与 Orca 的关系？** 首个可用版本必须独立替代其多 worktree 并行开发入口；Viva 自己管理任务 worktree、交互终端与恢复，并保留成员/授权/历史语义。可复用 Orca 开源代码或底层库，但 Orca 应用不是运行依赖，见 [首版投入使用门槛](first-usable-version.md)。
