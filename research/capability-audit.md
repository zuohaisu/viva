# AIO-1：复用优先能力审计

> **状态说明（2026-09-27）**：本审计针对的代码（`src/ticket_autopilot/` 的 Engine 与 Plane client）已于 ADR 0008 退役并从仓库删除；本文保留为历史证据，其中的复用建议不再适用。


**范围：** AIO-1 / R0；仅审计，不实现 Connector、QA、PR、CLI 接线或任何生产代码。  
**结论状态：** 已有 Engine 能解释一个声明式 Plan → Execute → Verify → Close 循环，但尚不能安全地把真实 AIO ticket 变成「有确定性证据、独立 QA、等待人工合并的 PR」。应复用 Engine、Plane REST client 和本机 `gh`，以薄 Connector / 确定性服务补齐缺口，而不是另建平台。

## 证据与核验记录

下表是本文所有「已有能力」的核验依据。路径均于本次审计以 `test -f` 核验存在；不以旧 `tasks/ticket-autopilot-v0.1-tasklist.md` 的架构断言为依据。

| ID | 核验方式 | 结果 / 可据此断言的边界 |
|---|---|---|
| E1 | `Read src/ticket_autopilot/engine/engine.py` | `Engine` 建立 forward `needs`，解释 `when`、`kind: loop`、`max_retries`，并在 loop 后将目标置 stale 重跑。 |
| E2 | `Read src/ticket_autopilot/engine/drivers.py` | 已实现四个实际 dispatch：`llm`、`cli`、`hermes`、`script`；`cli` 以 `_resolve_within_root` 拒绝越出允许根目录的 `cwd`，默认 `read-only`，传递 `--allowedTools`。 |
| E3 | `Read src/ticket_autopilot/workflows/ticket-pipeline.yaml` 与 `ticket-pipeline-hermes.yaml` | 两个 YAML 都定义 plan/execute/verify/close 和 reject → execute loop；标准版为 llm/cli/llm/script，Hermes 版前三者为 hermes。 |
| E4 | `Read src/ticket_autopilot/engine/handlers/close_ticket.py` | 唯一直接 import `plane_client` 的 Engine handler；成功时评论写回并 `set_state(..., "done")`。 |
| E5 | `Read src/ticket_autopilot/reference/ticket-pipeline/plane_client.py` | 可见 `list_issues`、`get_issue`、`create_issue`、`set_state`、`add_comment`、`delete_issue`；请求使用 `X-API-Key`、browser UA，PATCH body 使用 `state`。 |
| E6 | `python3 -m unittest discover -s src/ticket_autopilot/engine/tests -v` | **7/7 通过**：mock retry loop、CLI 根目录护栏、Hermes dispatch/loop。指定的 `python -m ...` 在本机因无 `python` 可执行文件退出 127；可用的 `/usr/bin/python3` 已完成同一测试发现。 |
| E7 | `command -v gh`、`gh --version`、`gh auth status`、`gh pr create --help`、`gh run list --help` | `gh 2.92.0` 已安装、已认证（`repo`、`workflow` scope）；帮助明确支持创建 PR 与列出 workflow runs。未执行创建 PR、push 或 CI 写操作。仓库无 `.github/` workflow 文件。 |
| E8 | `find .../connectors .../services .../schemas -maxdepth 1 -type f -exec wc -c`，并 Read 三个 `__init__.py` | 三目录各仅一个说明性 `__init__.py`，没有实现模块或 schema 文件。 |
| E9 | `Read src/ticket_autopilot/cli.py` | 产品 CLI 仅解析 `run/status/cancel/cleanup` 并打印 `[stub]`；源码注释为 `TODO(T1/T16): wire to controller`。 |
| E10 | `Read .../plane_client.py`、`reference/ticket-pipeline/README.md`、`stepB_verify_plane_api.py` | 代码与参考记录一致：Cloudflare 对非浏览器 UA 403/error 1010；REST client 以 browser UA 绕过；参考 smoke 设计为 create → done → get → delete。为遵守 R0，本次**未**对 Plane 发起任何请求。 |

## 7 阶段映射

| 闭环阶段 | 已有能力（证据） | 缺口 | 最小胶水建议 |
|---|---|---|---|
| **1. 工单接入与契约**（Ticket intake & contract） | 工作流已声明 `params.ticket_id`（E3）；Plane client 可列举或读取 issue（`list_issues` / `get_issue`，E5）。 | Engine 只把 UUID 给 planner，不读取 issue 内容，也没有 ticket contract、风险分级、AC verification 或 schema 校验。client 的项目 ID 还是错误项目（见已知坑 P1）。 | AIO-2 定义并校验一个 Plane issue → `ticket-spec` 适配层；从配置传 project ID，读取后只向 Engine 注入已校验契约。不改 Engine DAG。 |
| **2. 计划**（Plan） | `planner` 节点已由 `driver: llm` 覆盖；Hermes workflow 可替换为 `driver: hermes`（E2、E3）。 | 当前输入只有 ticket ID，未保证 planner 收到结构化需求、范围和验收标准；计划输出没有承重 schema 或落盘证据。 | 将已校验 ticket contract 作为 planner 输入，保存 `plan.md` / run artifact；计划生成仍复用已有 driver。 |
| **3. 执行/开发**（Execute） | 标准 workflow 有 `executor` 的 `driver: cli`，`cwd: ./sandbox`、`permission_mode: read-only`、工具白名单 `Read/Glob/Grep`（E2、E3）。 | 该配置**不能写代码**；Engine 不建 worktree/branch，不运行可写 Developer agent，不检查 diff/commit，也不 push。参考 pipeline README 也明确 Execute 目前只产出结果描述。 | 复用 `cli_call` 的目录/工具护栏，添加受配置约束的 worktree + feature branch + 可写开发调用 + git evidence service；禁止 main push。此为小型执行 Connector，不是新平台。 |
| **4. 确定性验证与独立 QA** | `verifier` 能解析 LLM JSON 并输出简化的 `accept/reject`（E2、E3）；测试已验证 Engine 可按 verdict 驱动（E6）。 | 没有执行 ticket 所列命令、收集 exit code/diff/commit/CI；Verifier 不是独立 Codex QA，也没有 `qa-verdict.json`、schema 校验、PASS/FAIL/BLOCKED 语义或证据。 | AIO-7 实现只读 Codex QA Connector 和 `qa-verdict` schema；另以确定性 service 运行 required checks、保存 stdout/exit code。任何畸形/缺证据均映射 BLOCKED，不可 accept。 |
| **5. 有界修复循环**（Bounded fix loop） | YAML 有 `verify → execute` loop，条件为 `nodes.verify.decision == 'reject'`，`max_retries: ${vars.max_retries}`；两个 workflow 的值均为 5（E1、E3），mock test 证明 reject 两次后第三次 execute 并 close（E6）。 | 上限 5 与 v0.1 的最多两次 fix 不一致；没有把结构化 QA finding、attempt、blocked/exhausted 状态交给开发者；max retries 耗尽后没有确定性 BLOCKED 结果。 | 保留 Engine loop，实现 policy adapter：仅 QA FAIL 可回环、`max_fix_attempts=2` 由 ticket/config 注入、耗尽或 QA BLOCKED 显式写为 BLOCKED 并通知人。 |
| **6. Pull Request 与 CI 证据** | 原生 `gh` 已安装/认证，命令面具备 `gh pr create` 与 `gh run list`（E7）。GitHub Actions 可作为 GitHub 原生 CI 载体，但本仓库当前没有 workflow 文件（E7）。 | Engine/workflow 没有 branch、commit、push、PR、CI node 或证据采集；不能声称已有 PR/CI 闭合。当前 `gh` 不是 MCP 连接。 | AIO-8 以确定性 Git/GitHub Connector 在验证通过后执行 branch/commit/push/`gh pr create`，轮询或读取 required CI evidence；只创建 PR，绝不 merge。优先复用现有 `gh` 与仓库 CI 配置。 |
| **7. 工单状态与结果**（Ticket status & result） | `closer` 的 script driver 调用 `close_ticket`；该 handler 复用 Plane client 写 plan/result 评论并置 Done（E4、E5）。 | 仅有 accept → Done；没有 In Progress、In Review、Blocked、Cancelled 映射、PR 链接/QA/CI evidence，也没有错误状态处理。且受 P1 的错误 project ID 影响。 | AIO-6 将 Plane 状态和项目配置参数化，提供最少的 `in_progress` / `in_review` / `blocked` / comment 回写；只由确定性结果转换状态。 |

## 横切关注

| 关注 | 已有能力（证据） | 缺口 | 最小胶水建议 |
|---|---|---|---|
| **人工 Review / Merge 安全门** | `gh` 可创建 PR（E7）；项目原则与规格要求人 review 后才 merge。当前源码没有 `gh`、git merge 或自动 merge 调用（E1–E5、E9 的源码审阅）。 | 尚未创建 PR，也未把 review / required checks 配置为安全门；无法仅凭本地审计证明远端 branch protection 已启用。 | PR 一律 Draft/待 review；在 GitHub 仓库显式配置 branch protection / required checks，由人工 merge。Controller/agent 不实现或调用 merge。 |
| **安全边界** | CLI driver 实际强制受限 cwd，默认 read-only，并传递工具白名单；越界 cwd 有单测覆盖（E2、E6）。Plane token 从 `PLANE_API_KEY` 或用户目录配置读取，未硬编码在源码（E5）。 | `executor` 的 read-only 配置与真实开发写入需求相冲突；无工作树隔离、main push 拦截、风险 tier、生产/迁移/secret policy 的运行时验证。Hermes variant 的「caller enforced」sandbox 在本仓库没有独立强制实现。 | 仅给专用 disposable worktree 开发写权限；明确 allow-main-push/merge/production/migrations 均为 false，凭证只经环境变量、Keychain 或 `gh` auth；在 deterministic preflight 检查 forbidden paths、risk tier 与命令白名单。 |

## 已核验的可复用能力清单

1. **DAG 与重试解释器**：`src/ticket_autopilot/engine/engine.py`（E1）。可直接复用为控制流，不应重写状态机平台。
2. **四类 drivers**：`src/ticket_autopilot/engine/drivers.py`（E2）。其中 CLI 的 `SecurityError`、目录沙箱、read-only 和 allowlist 是现成安全基线。
3. **声明式闭环定义**：`src/ticket_autopilot/workflows/ticket-pipeline.yaml` 与 `ticket-pipeline-hermes.yaml`（E3）。前者适合明确的 LLM/CLI 接线，后者证明 Hermes 可替换推理节点。
4. **Plane 回写节点**：`src/ticket_autopilot/engine/handlers/close_ticket.py`（E4）。这是当前 Engine 中唯一接触 Plane 的位置，适合继续保持绑定局部化。
5. **Plane REST client**：`src/ticket_autopilot/reference/ticket-pipeline/plane_client.py`（E5、E10）。包含读取、创建、状态、评论、删除方法与正确 PATCH 字段；只能在 project ID 参数化后用于 AIO。
6. **确定性 Engine 自测**：`src/ticket_autopilot/engine/tests/test_engine.py`（E6，7/7 绿）。它验证的是 Engine/driver 护栏和 mock loop，**不是**真实 Plane、真实代码开发、PR 或独立 QA 的验收。
7. **原生 GitHub 面**：本机 `gh` + GitHub Actions（E7）。`gh` 可作为薄 GitHub Connector；当前无 MCP 连接、无本仓库 Actions workflow，不能将它误写成已经接线的 PR/CI 自动化。
8. **已声明的 MCP / 通知能力边界**：AIO-1 任务契约声明 Plane（`mcp__plane__*`）、Linear（`mcp__linear__*`）与 agent-mail 已连接。本次执行 harness 仅暴露文件/命令工具，未暴露这些 `mcp__*` namespace，因此没有进行 MCP 调用验证；尤其 Plane MCP 已有 403 证据（P2），不得作为 AIO 的主 Connector。Linear/Mail 可在后续环境中用作只读 intake 或通知候选项，须先做实际调用验证后才可成为承重路径。

## 已知坑与最小处理

| ID | 已知事实与证据 | 风险 | 最小胶水 / 处理 |
|---|---|---|---|
| P1 | `plane_client.py` 的 `PROJECT_ID` 硬编码为 `5c5c7207-868e-4da6-ae95-66e7d02eeebb`（E5）；AIO 目标项目为 `d40168f5-5d44-4810-a39e-3b6558e9bf6e`。 | 会读写错误项目，尤其 close 路径危险。 | project ID 不得模块常量硬编码；从受校验配置/connector 实例注入，并以只读 get/list preflight 比对 workspace/project。 |
| P2 | Plane client 与参考 README 记录 Cloudflare 对非浏览器 UA 返回 403 error 1010，MCP proxy 因 UA 触发此问题；REST client 有 browser UA（E5、E10）。 | Plane MCP 不能作为可靠主路径；盲目重试会制造假故障。 | 后续 Plane Connector 复用 REST client 的 UA、鉴权与错误处理，不依赖 MCP proxy；先以无副作用读取检查连接。 |
| P3 | 当前 Verifier 仅 llm/hermes `{"decision":"accept"|"reject","reason":...}`（E3）；规格 §8.2/§9 要求独立 Codex QA 和 schema 化 `qa-verdict.json`。 | LLM 自述可能放行，没有独立证据，违背 no-false-success。 | AIO-7 增加只读 Codex QA、严格 schema validator、AC-by-AC evidence 和 BLOCKED 映射；未验证 verdict 不可到 accept。 |
| P4 | Engine 没有 git/`gh` 调用或 PR node（E1–E4）；`gh` 只是本地已验证可用命令面（E7）。 | 不会产生 branch/commit/PR/CI evidence。 | AIO-8 用最小 deterministic GitHub Connector 驱动 worktree→branch→commit→push→PR→CI evidence；禁止 auto-merge。 |
| P5 | `src/ticket_autopilot/cli.py` 输出 `[stub]` 并标记 TODO（E9）。 | 产品入口看似成功却未启动任何闭环。 | 只在 intake、run state、connector 和 blocked semantics 均存在后接到 Engine；在此之前保留 stub，不能以 exit 0 当成功。 |
| P6 | `connectors/`、`services/`、`schemas/` 各只有说明性 `__init__.py`（E8），即**空脚手架**。 | ticket contract、QA verdict、Git/Plane adapters 和确定性验证均未实现。 | 按 ticket 增加最小的单一职责模块；复用 Engine/drivers/Plane REST/gh，不创建第二个 orchestrator。 |
| P7 | Engine workflow 是 `verify accept → close`，而 v0.1 目标顺序需要确定性验证、PR/CI evidence、独立 QA、人工 review（E3；规格 §11、§14）。 | 当前顺序会在没有 PR/CI/独立 QA 时关闭 ticket。 | 调整后续 workflow composition，使确定性失败/QA BLOCKED 均留 ticket 开放并标 BLOCKED；只有获得所需 evidence 后转 In Review，不直接 Done。 |
| P8 | workflow 的 retry cap 为 5（E3），规格 §15 的自动 fix 上限为 2。 | 与明确的有界修复政策不一致。 | 将 cap 作为已校验 policy 参数，v0.1 设置为 2；耗尽写 BLOCKED_QA_EXHAUSTED 并停止。 |

## 不新建平台的结论

以下范围**不需要新平台**：

- DAG、节点依赖、条件边、retry 计数和 run snapshot：复用 Engine。
- Plan 与现有 CLI/LLM/Hermes 调用：复用 drivers，补入经过校验的契约输入。
- Plane 的 issue 读取、评论与状态更新：复用 REST client 的已验证请求实现，修正为参数化项目配置。
- PR 创建、CI 查询与 GitHub 认证：复用本机 `gh` 和 GitHub Actions；不把它们重做成 API 平台。
- 人工 review/merge：复用 GitHub PR 与 branch protection；系统只停止在待 review，绝不自动 merge。

真正必要的自定义范围只是把上述能力可靠地串为证据链的**薄 Connector / deterministic policy services**：Plane intake/status 配置、ticket contract validator、受限 git/worktree + verification、Codex QA verdict validator、GitHub PR/CI evidence、以及产品 CLI 的调用编排。Engine 已经是最小 controller；不要新建第二个 controller 或持久化平台，更不实现 resume、webhook、并行 run、自动部署或自动 merge。

## 下一步最小胶水优先级

1. **AIO-2：契约与状态语义先行。** 固化 Plane ticket → `ticket-spec`、R0/R1 preflight、不可用时 BLOCKED；这使 planner 不再只拿 UUID。
2. **AIO-6：Plane Connector。** 参数化 project ID，使用 REST/UA 路径，先实现只读 intake 与确定性状态/评论回写；不要尝试修 MCP proxy。
3. **AIO-7：确定性验证 + 独立 QA。** 先有 command exit/diff/commit evidence，再接只读 Codex 和 schema 化 `qa-verdict`；把现有 accept/reject loop 映射为 PASS/FAIL/BLOCKED，并将 cap 收紧到 2。
4. **AIO-8：GitHub PR/CI 证据。** 在确定性验证通过之后，薄封装 git/`gh` 创建 draft PR、采集 CI；不实现 merge。
5. **后续 CLI 接线。** 仅把已验证的上述闭环能力暴露为 `run/status/cancel/cleanup`；不可把当前 stub 或 mock loop 宣称为真实交付。

完成这些薄层后，用一张真实低风险 R0 ticket 进行纵向切片：任何 API、证据或 QA 门禁失败均标为 `BLOCKED`，不标成功。
