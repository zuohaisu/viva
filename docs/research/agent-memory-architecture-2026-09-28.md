# Viva 的 AI 记忆架构研究：OpenViking、Claude Code、MemGPT / Letta

核查日期：2026-09-28。状态：**Research / Proposed，尚未裁决、未集成、未实测**。这是本轮记忆选型的统一比较报告；[ADR 0011](../decisions/0011-rust-host-and-tui.md) 仍是 Rust / Pi / SQLite 的唯一权威技术结论。[Holographic 专项研究](hermes-holographic-memory-2026-09-28.md) 保留其源码证据，不构成批准或本轮优先顺序。

## 1. 推荐与边界

推荐 **有界工作上下文 + 可导航的分层长期记忆 + 明确策展/有效性规则 + 混合检索**。向量可以参与召回，不决定事实真伪、长期重要性、权限或当前有效性。工作/短期/长期描述信息的使用方式，不必建三个独立数据库。

成品路线优先验证 OpenViking：它面向跨 harness 上下文，有现成 Pi 等适配；不得把“优先验证”写成已接受后端。Claude Code 与当前 Letta 提供重要的轻量文件导航参考，MemGPT 提供上下文分层/调页参考；不因借鉴而把整个认知循环换成另一个 harness。Holographic 仍是轻量基线候选，但不能因为用户已经使用就免于召回、归档与隔离比较。Python 历史与迁移成本不参与淘汰。

本轮推进的是 knowledge 的研究证据，不是运行能力：没有安装候选、接入 Pi、调用记忆模型、读取个人记忆或跑性能基准。

## 2. 第一性问题：存得下，不等于记得对

一个可用的长期记忆系统必须分别回答：记录什么；如何组织；这一轮如何找到；哪些仍有效/允许使用；怎样装进有界上下文；怎样追溯、修正与退出活跃状态。

语义相似度只回答查询相关性的一个维度。例如大量“在 A 公司工作”的旧记录可能很相似，但一条新的“已经离职”决定当前答案。一个极少被读取的授权限制可能很重要；反复读取不证明事实正确或值得常驻。数据增大不是向量索引必然失败的证明，但重复、冲突、时效未处理时，纯相似度 top-k 会竞争有限窗口。向量数据库可以支持元数据过滤；这些问题并非所有向量产品都无法补足，而是不能只靠距离度量解决。

[Generative Agents 原论文](https://arxiv.org/html/2304.03442v2#S4.SS1) 将 relevance、recency、importance 分开；其中 importance 由模型打分，属于仿真人物实验，不保证 Viva 决策正确。Viva 可以借鉴分离信号，不照搬固定权重和情感重要性。检索分数、可信证据、使用收益不能混成一个 trust 值。

[LongMemEval](https://arxiv.org/abs/2410.10813v2) 把信息提取、跨会话推理、时间推理、知识更新和未知时拒答分别评估，并区分 indexing / retrieval / reading。其研究也提醒：只保留抽出的单句事实可能丢失上下文，取回正确内容后还需正确阅读。因此应保留可回查的原始证据，不把摘要当唯一真相；论文结果不能外推成 2026 年所有模型的固定性能。

## 3. 各方案核查

### 3.1 OpenViking：目录结构 + 分层内容 + 语义检索

固定源码 [`a09a9d20a8e07d08973aee177802d00e08df29e6`](https://github.com/volcengine/OpenViking/commit/a09a9d20a8e07d08973aee177802d00e08df29e6)，提交于 2026-09-26。它是 Python 服务与文件系统 binding 等组成的上下文后端，不是一个 Rust 纯库；可以共享一个服务。资源、记忆、技能都有可导航目录。L0 是短摘要，L1 是概要，L2 是原文/详细内容；这些层级表达粒度，不等于重要性等级。[分层源码文档](https://github.com/volcengine/OpenViking/blob/a09a9d20a8e07d08973aee177802d00e08df29e6/docs/en/concepts/03-context-layers.md)、[依赖](https://github.com/volcengine/OpenViking/blob/a09a9d20a8e07d08973aee177802d00e08df29e6/pyproject.toml)

它仍需要 embedding。`find` 直接查询，`search` 根据当前会话做意图分析，再定位向量起点、递归搜索目录并可选重排；可导航结构和语义候选互补。LLM 摘要可能遗漏，超大目录的采样也应测试覆盖，不能用目录摘要代替原文。[检索说明](https://docs.openviking.ai/en/concepts/07-retrieval)、[检索源码](https://github.com/volcengine/OpenViking/blob/a09a9d20a8e07d08973aee177802d00e08df29e6/openviking/retrieve/hierarchical_retriever.py)

会话 commit 保存原对话并异步提炼；记忆候选经过预筛与 LLM 创建/跳过/合并/删除。`memory_diff.json` 留新增、修改前后与删除内容，可以用于审计，但不等于完整 active→superseded→archived 与恢复流程。当前还提供 memory consolidation 合并/拆分/压缩；模型提示里的“不虚构/保留事实”是目标，不是准确性保证。[会话](https://docs.openviking.ai/en/concepts/08-session)、[写回源码](https://github.com/volcengine/OpenViking/blob/a09a9d20a8e07d08973aee177802d00e08df29e6/openviking/session/memory/memory_updater.py)、[整理](https://github.com/volcengine/OpenViking/blob/a09a9d20a8e07d08973aee177802d00e08df29e6/docs/en/context-compilation/06-memory-consolidation.md)

官方仓库已有 [Pi](https://github.com/volcengine/OpenViking/tree/a09a9d20a8e07d08973aee177802d00e08df29e6/examples/pi-coding-agent-extension)、[Claude Code](https://github.com/volcengine/OpenViking/tree/a09a9d20a8e07d08973aee177802d00e08df29e6/examples/claude-code-memory-plugin)、[Codex](https://github.com/volcengine/OpenViking/tree/a09a9d20a8e07d08973aee177802d00e08df29e6/examples/codex-memory-plugin)、[Hermes](https://github.com/volcengine/OpenViking/tree/a09a9d20a8e07d08973aee177802d00e08df29e6/examples/hermes-plugin) 接口，是真实复用优势，但不是已在 Viva 跑通。Pi 插件默认 context takeover 会用会话概要替换已覆盖旧轮次，且每轮 capture/召回；建议首个验证关闭 takeover，只验证长期记忆，继续由 Pi 管工作上下文。是否将来交由它接管另行裁决。[Pi 配置与实现说明](https://github.com/volcengine/OpenViking/blob/a09a9d20a8e07d08973aee177802d00e08df29e6/examples/pi-coding-agent-extension/README.md)

默认 git origin 派生 workspace peer，broad recall 可以召回其他 workspace；actor scope 限制 peer，但仍允许用户全局/账号共享内容。peer 不等于 Viva Resident，repo 不等于 Workspace。正式隔离需 account/user API key 和 ACL 或受控网关；dev 模式所有请求是 ROOT。明确 api_key 模式无 root key 在当前源码会启动失败，不能照文档简写误认为自动 dev。最小接入须校验成员/项目映射和 live grant，不把管理凭证给 Worker。[多租户](https://docs.openviking.ai/en/concepts/11-multi-tenant)、[ACL](https://github.com/volcengine/OpenViking/blob/a09a9d20a8e07d08973aee177802d00e08df29e6/openviking/storage/acl.py)、[api_key](https://github.com/volcengine/OpenViking/blob/a09a9d20a8e07d08973aee177802d00e08df29e6/openviking/server/auth/plugins/api_key.py)、[dev](https://github.com/volcengine/OpenViking/blob/a09a9d20a8e07d08973aee177802d00e08df29e6/openviking/server/auth/plugins/dev.py)

官方 Intel macOS helper 只证明有对应分发路径；其 benchmark 是作者在指定模型/数据集上的报告，不能当 Viva 的中文任务、低内存 Intel Mac 或 16 路运行证据。[官方入口及基准说明](https://github.com/volcengine/OpenViking/tree/a09a9d20a8e07d08973aee177802d00e08df29e6)

### 3.2 Claude Code：工作上下文、短入口与按需文件

Claude Code 的工作上下文包括对话、工具结果、读入文件、说明与笔记。接近上限会清理旧工具输出并总结；摘要可能丢早期细节。根 CLAUDE.md、无条件规则与 auto memory 在 compaction 后从磁盘重载，按路径的规则/嵌套说明随相关文件再读。应复用各 harness 已有的会话与压缩能力，不重写一套 token manager。[工作原理](https://code.claude.com/docs/en/how-claude-code-works#the-context-window)、[compaction 恢复](https://code.claude.com/docs/en/context-window#what-survives-compaction)

持久记忆主要是 Markdown：CLAUDE.md 保存人写的说明；auto memory 保存模型选择的长期笔记。启动只读 MEMORY.md 的前 200 行或 25KB（先到者），主题文件按需读取；同一 repo 的 worktrees 共用本机记忆目录。客户端提醒模型压缩超限索引，但预算不证明所有重要内容都被保留。该机制不是向量数据库；官方未公开一个能保证重要性判断正确的完整算法。[官方 memory 文档](https://code.claude.com/docs/en/memory#auto-memory)

普通 subagent 有独立工作上下文，可设置自己的持久目录；它不是把所有主会话记忆自动共享出去的成员系统。harness 的目录或提示词不能代替 Viva 的 Resident 与授权。[官方 subagents](https://code.claude.com/docs/en/sub-agents#enable-persistent-memory)

优势是开放文件、轻量导航、按需读取；代价是模型探索可能增加工具调用/延迟，目录和标题差时会找错或漏读。Anthropic 明确说明按需探索和预先检索存在取舍，可混合使用。[官方 context engineering](https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents)

### 3.3 MemGPT 原论文与当前 Letta：不能混为一个版本

[MemGPT 原论文 v2](https://arxiv.org/html/2310.08560v2) 的主上下文分只读系统指令、可编辑的固定工作区、近期 FIFO 消息；外部 recall 保存对话，archival 保存长期材料。模型用工具读写/检索，将所需记录换入窗口；队列压力触发告警、淘汰与摘要，heartbeat 支持连续工具步骤。它解决的是有限窗口与主动调页。论文承认正确结果可能排得很后、agent 提前停止翻页、弱工具调用能力影响效果；不是“无限准确记忆”的证明。

当前 [`letta-ai/letta`](https://github.com/letta-ai/letta) 已将活跃开发指向 Letta Code，archive 分支为 retired V1 API server。不能将 legacy core/archival 的数据库配置和默认值说成现在唯一架构。[V1 层级文档](https://docs.letta.com/v1-sdk/memory/context-hierarchy) 仍可作历史参考。

今天的 [MemFS](https://docs.letta.com/concepts/memfs) 是 Git-backed Markdown：默认不带语义/向量索引，用文件搜索和读取；可选 search mod 加语义/混合检索。文件版本历史、修改并行与备份是可复用优势；Git 不自动表达某事实何时失效。

固定当前源码 [`1cab1b78d413789cf77c852a884aee47eada6007`](https://github.com/letta-ai/letta-code/tree/1cab1b78d413789cf77c852a884aee47eada6007) 支持 v2 根 MEMORY.md 索引、根 Markdown 核心、子目录延迟索引；官方 MemFS 页面仍描述 v1 的 system/ 布局。新建本地 agent 且启用 MemFS 的路径会创建 v2；不能推断历史或禁用实例都是 v2。选择具体版本时应以源码/真实配置核查，而不照抄 mutable 布局。[格式](https://github.com/letta-ai/letta-code/blob/1cab1b78d413789cf77c852a884aee47eada6007/src/agent/memory-format.ts)、[创建路径](https://github.com/letta-ai/letta-code/blob/1cab1b78d413789cf77c852a884aee47eada6007/src/backend/local/local-backend.ts#L318)、[上下文编译](https://github.com/letta-ai/letta-code/blob/1cab1b78d413789cf77c852a884aee47eada6007/src/backend/local/system-prompt-compilation.ts)

目前没有查到可直接插进 Pi 的官方独立本地 MemFS query/write provider：已有 CLI 状态、diff、备份/导出能力可复用；Hosted MCP 是创建/委派给完整 Letta agent，不是仅提供记忆。让 Pi 通过已有工具读写实际 Git/Markdown checkout 是薄适配候选，核心装配、并发与权限仍需验证。[CLI](https://docs.letta.com/platform/cli/reference)、[Hosted MCP](https://docs.letta.com/platform/hosted-mcp)

当前 dreaming 可整理经验，第二轮 review 明确增加 token；它不是 2023 论文机制。可以借鉴整理/版本维护，但模型仍可能写错或过度总结。当前本地 CLI 可 in-process，不要求运行旧 PostgreSQL 栈；完整 Letta 是另一套 harness，不能因“只想用记忆”就默认全引入。[整理机制](https://docs.letta.com/configuration/memory)、[本地运行](https://docs.letta.com/self-hosting)、[当前包依赖](https://github.com/letta-ai/letta-code/blob/1cab1b78d413789cf77c852a884aee47eada6007/package.json)

### 3.4 Holographic：轻量事实库基线

已核实 bundled Hermes 实现是 SQLite + FTS5 候选 + Jaccard / 可选 HRR 重排 + 使用反馈；HRR 是确定性的词/关系向量表示，不是训练语义模型。默认不自动提取会话记忆，删除是 SQL DELETE，缺少可恢复归档和成员/项目授权层。详见[固定源码与版本研究](hermes-holographic-memory-2026-09-28.md)。

低模型调用开销值得测，但中文同义、跨会话综合、旧事实被新事实取代是重点风险。它的可用性不能由“全息”命名或用户体验推导；用户实际安装版本仍未核查。

## 4. 对 Viva 的比较

| 方案 | 最值得复用的能力 | 不能直接替 Viva 解决的部分 | 本轮定位 |
| --- | --- | --- | --- |
| OpenViking | 可检查目录、L0/L1/L2、语义检索、会话提炼、多个 harness 适配 | Viva 授权/归属、可恢复失效、重要性可靠性、目标机器预算 | **优先验证的成品候选** |
| Claude Code 文件记忆 | 短入口、主题文件按需读、上下文压缩、subagent 隔离工作细节 | 跨 harness 成员连续性、全局知识治理、结构化时效与来源 | 当前 harness 能力 + 设计参考 |
| MemGPT / Letta | 有界核心与外部存储、检索/改写工具；当前文件记忆与版本历史 | 自主选择不保证重要性正确；完整 harness 接入不等于独立 memory provider | 分层方法参考；MemFS 薄适配候选 |
| Holographic | 本地事实存储、FTS / HRR、使用反馈 | 中文语义、跨会话综合、归档、作用域/授权 | 轻量可测基线，不因已使用而优先 |
| 原始日志直接全部向量化 | 可复用语义候选召回 | 准入、冲突、来源、有效性、窗口预算仍需另做 | 不作为完整记忆架构 |

## 5. 建议的产品契约：提案，未实现

### 5.1 四种使用层次

| 层次 | 内容 | 使用策略 |
| --- | --- | --- |
| 核心资料/约束 | 成员身份引用、用户明确的稳定偏好、有效关键约束 | 短且有预算；不靠相似度碰运气；身份/权限不能被模型笔记静默改写 |
| 工作记忆 | 当前 Task 的目标、决策、未完成项、证据、授权引用与分支 | 为一次工作装配；权威任务/授权从 Viva 查询，避免摘要冒充最新事实 |
| 近期入口 | 活跃项目摘要、最近有用的经验、未结事项与详细来源索引 | 有界、随任务/时间复审，方便快速导航；它是视图/缓存，不是另一份可独立改写的事实库 |
| 长期知识/原始证据 | 策展条目、技能、项目决定，另有完整来源记录 | 正文按需读；active / superseded / archived / retracted 要可区分，退出活跃不等于物理删除 |

工作上下文由 Pi/其他 harness 持有；Viva 提供同一语义的成员资料、任务简报和受控知识入口。当前任务状态、授权状态、仓库 HEAD 等动态事实应查询权威来源，不从旧记忆猜测。谈话分叉各有工作包和来源引用，分支里未验证的设想不能直接变成所有成员的长期事实。

### 5.2 写入与维护

记忆准入保留当前 temporal-model 的原则：用户强调、再次需要、意外发现或重新推导昂贵；不能把每条 terminal 输出/每次对话自动晋升。模型可提出摘要/经验候选；来源、scope、授权和生命周期由受控接口校验。观察、决定、推断、待验证假设应区别保存。

每条知识至少可追溯：owner/scope、来源事件或文档版本、记录时间、适用时间/复审条件、状态、替代关系、重要性理由、真实使用证据。这里是需求清单，不强制统一成某个新 schema；先验证后端已有能力。修改保留旧版本与理由，多 worker 同时修改要有版本冲突处理，不能最后写入静默覆盖已批准约束。

“重要”至少区分：用户明确 pin 的约束、当前 Task 必需、再获得成本高、后续实际有用；最近读得多只是一个弱信号。稳定约束不按普通时间衰减自动淘汰；短期状态需要到期/任务结束复审。模型打分可参与候选排序，不覆盖权限、有效性或 owner 的明确决定。读到、取回、使用、使用成功应区分；使用收益不证明真伪。

对失效知识，保留历史来源，但默认当前事实查询不返回为有效结论；问“过去”时按时间回查。项目结束或角色变化可退出活跃范围，不能把整个过去物理抹掉。执行清理仍须遵守已有 worktree / 历史数据保留规则，本提案不授权删除任何数据。

### 5.3 检索与上下文装配

推荐顺序：授权与成员/项目范围 → 当前适用状态/查询时间 → 精确字段、FTS、目录导航与可选向量候选 → 按任务相关性/明确重要性/来源/时效去重与排序 → 在 token 预算内装配 → 需要时按引用展开原文。

必须做到：关键有效约束无需语义搜索即可拿到；精确名称/ID 不只靠 embedding；中文同义查询可走语义路径；冲突必须显示来源和适用时间，不能把两条相反事实融合为模糊摘要。摘要/索引是导航或派生物，不是原始证据的替代品。检索权限在存储/接口层执行，不只写在 prompt 中。必要时对候选进行最终授权和版本复核；不能宣称某个插件默认设置已满足这个契约。

SQLite 保存 Viva 状态和关系，文件/外部后端持有知识正文，各事实只有一个权威来源。若采用 OpenViking，Viva 只保存需要的归属、授权、生命周期与稳定引用，不再维护可独立修改的同一正文。目录摘要、FTS/向量索引等派生数据应可重建；能否满足版本/失效语义需集成验收。关系需要增强时可先复用 SQLite 的关系能力，不在缺口未经证明时另建图数据库。

## 6. 资源与成本

截至核查日，没有从已查官方渠道找到 OpenViking 固定 hosted 月费；按自托管开源方案估算。当前主项目 AGPLv3，Rust CLI/examples 有各自 Apache 2.0，Hermes 示例 MIT；不能按旧文章把全项目算作 Apache。本文只报告许可事实；实际复用代码/部署方式的合规核查在实施前处理，不将第三方代码重新标成 Viva 的 MIT。[许可](https://github.com/volcengine/OpenViking#license)

其总成本包括 CPU/RAM/存储、embedding、摘要/抽取/整理/查询规划的 LLM、可选 reranker 和主 Agent 读入记忆的 token。自托管 OpenViking 不自动产生另一项方舟云知识库 CU 费。

2026-09-28 [官方模型价格](https://docs.volcengine.com/docs/82379/1544106) 给出的人民币常规在线推理示例：

| 模型用途 | 单价（每百万 tokens） | 条件 |
| --- | --- | --- |
| doubao-embedding-vision 文本 | ¥0.70 | 图片价格不同 |
| doubao-seed-2.0-pro 输入 / 输出 | ¥3.2 / ¥16 | 每次请求输入 ≤32k，非音频；更长请求有更高档位 |

显式假设：每月文本 embedding 10M tokens，记忆处理 LLM 输入 5M、输出 0.5M，各请求均满足上表条件，无缓存、图片、重排或重试，则 `10×0.70 + 5×3.2 + 0.5×16 = ¥31/月`。这只是算术场景，不是 Viva 用量预测或固定套餐；未包含主 Agent 和后端资源。档位按单次请求长度，不按月累计 tokens；免费额度不是永久零成本。模型选择与调用频率须记录后再估算实际账单。


记忆的总成本还包括：检索 query embedding / rerank、每轮常驻上下文、读取正文后的输入、摘要/提炼/维护、失败重试与索引更新。Claude 的 prompt cache 是计算复用，不是长期记忆，也不减少已加载内容所占的窗口。缓存费率/TTL 按所选模型和供应链核查，不能以 API 价格推导订阅账户的实际账单。[Claude Code 缓存](https://code.claude.com/docs/en/prompt-caching)、[官方成本说明](https://code.claude.com/docs/en/costs)

16 个 Worker 不需要 16 份完整记忆索引或 16 个本地 embedding 模型。候选若需服务，优先验证一份 Viva 运行期共享服务/索引与有界维护队列；每个 Worker 仍有自己的模型上下文和使用范围。共享不等于把私人记忆全开放。关闭 TUI 后停止新增工作和维护，保存未完成状态；无独立后台 daemon 承诺。若候选无法做到目标退出语义，应明确适配代价或淘汰。

不把本地模型当作必然便宜：Intel Mac 上模型权重与推理可能主导 RAM/CPU；API embedding 省本机推理资源但有调用成本/数据传输。先用配置和同负载测量选路径，不能因 Rust 宿主就宣称全部记忆组件低内存。

## 7. 复用优先：已检查能力与真实缺口

| 所需部分 | 已检查的能力 | 最小缺口/动作 |
| --- | --- | --- |
| 知识归属/来源/使用证据 | 当前 `src/viva/knowledge/registry.py`、ADR 0010 | 已有人工条目、owner、provenance、usage、retraction；缺自动召回、分层入口、时间/替代、完整归档；可按 Rust 目标替换，不要求保留 Python |
| 工作包 | Task brief、Viva grant 与执行记录；Pi/Claude compaction | 保留权威动态事实与有界交接；复用 harness 内部上下文管理 |
| 可导航长期存储/检索 | OpenViking、Claude 文件模式、当前 Letta MemFS、Holographic | 先测现成后端；必要薄适配连接 Viva 的作用域、来源、版本/退出语义；不自建 HRR、embedding 模型或 Agent 循环 |
| 重要性/策展 | temporal-model 准入门、后端 session提炼/整理 | 显式重要性理由、有效性、被替代关系；不会由一个检索分数自动产生可靠政策 |
| Pi/其他 harness 入口 | Pi 扩展、OpenViking 现有插件、Letta 文件接口 | 检查默认行为和受控接口；不重复建设已证明可复用的 adapter；默认 auto capture/takeover 不自动等于 Viva 合同 |

当前 registry 的筛选与撤回有测试，但会读取整个 JSONL 并返回范围内所有 active 条目；不是有界自动召回系统。对外只报告已有手工策展/归属/使用证据，不宣称自动学习已经存在。

## 8. 可证伪的验收与裁决顺序

以同一组测试材料、相同回答模型、相同上下文预算、相同输入/维护规则比较候选；分开测检索 Recall@k、最终回答正确性、误召回/过期回答、未知时拒答、来源准确性。不要混比纯检索命中和模型问答，也不要拿不同厂商的宣传分数直接排名。

必测场景：大量重复闲聊包围低频重要决定；离职/迁移/偏好变化后的当前与历史问题；同名项目/跨成员隔离；中文同义表达；跨会话聚合；失效与归档后回查；摘要遗漏后原文可找；并发写冲突；Pi 与第二个 harness 使用同一受控知识资产；重启与关闭后的恢复。

规模增长按相同分布逐级测量（例如 1 千 / 1 万 / 10 万条，数字是实验负载，不是通过门槛）。目标 Intel Mac 上覆盖空闲、正常少量 Worker、16 路峰值；分别记录宿主、后端、Worker与模型子进程的 RAM、memory pressure/swap、CPU，检索/上下文装配/任务响应 p50/p95、token/调用费、写入/维护时间、磁盘与索引增长。量化通过预算需根据目标机器与用户要求确定，不编造“已达标”。

裁决建议：先认可上述产品契约，再用最小真实任务验证 OpenViking 现成方案；若资源、有效性/归档、归属或上下文所有权不满足，明确缺口，再比较薄适配的文件/SQLite 方案与 Holographic。只有在同负载证据支持时批准具体后端。此次研究不改变 Rust / Pi / SQLite，也没有批准新记忆服务或自动写入策略。
