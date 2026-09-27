# Hermes + Holographic 外部记忆研究

研究日期：2026-09-28。状态：研究证据，不新增选型裁决，不表示 Viva 已接入记忆。推进 knowledge 能力的证据：定位同名项目、核对固定源码、指出接入缺口与验证方法。主技术决策仍是 [ADR 0011](../decisions/0011-rust-host-and-tui.md)。

后续比较见[统一记忆架构研究](agent-memory-architecture-2026-09-28.md)。本专项保留 Holographic 的机制证据；下文的复用建议不意味着它优先于后续核查的成品候选，也不因用户已使用而免于同负载比较。所有记忆后端建议仍未裁决。

## 结论

Hermes 里的 Holographic 是本地结构化事实记忆 provider：SQLite 持久化 + FTS5 关键词召回 + Jaccard/HRR 重排 + 信任反馈。它不是云服务，也不是必须常驻的向量数据库；“external”指 Hermes 内置 MEMORY.md / USER.md 之外的 provider。它可以作为跨会话记忆的复用对象，但不自动提供 Samuel 身份连续性、成员隔离、完整记忆生命周期或跨 harness 接口。[官方 provider 文档](https://hermes-agent.nousresearch.com/docs/user-guide/features/memory-providers)

## 项目定位与版本

| 项目 | 本次固定快照 | 范围与维护状态 |
| --- | --- | --- |
| [NousResearch/hermes-agent](https://github.com/NousResearch/hermes-agent/tree/6f7a7991bb069db07ae74a479823ce8310f8c7e0/plugins/memory/holographic) | HEAD `6f7a7991bb069db07ae74a479823ce8310f8c7e0` | 本次公开目录树仍有 bundled Holographic 源码与测试；本文机制分析以此为准 |
| [NousResearch/hermes-plugin-holographic](https://github.com/NousResearch/hermes-plugin-holographic/tree/246d9c723a79ea07bd7455bf4b364b44269526cf) | `246d9c723a79ea07bd7455bf4b364b44269526cf`，2026-09-17；包版本 0.1.0 | 独立 handoff 副本，说明明确不承诺 Nous 官方维护；原 bundled 存在时，同名用户插件被它遮蔽 |
| [bysc1000/holographic-memory](https://github.com/bysc1000/holographic-memory/tree/166ba35c5dd6973956dabee97165fc4eb0750b20) | `166ba35c5dd6973956dabee97165fc4eb0750b20`，2026-06-11；包版本 0.16.0 | 社区独立库及 Hermes 插件参考副本，不能将其增强功能视为 bundled 默认能力 |

移出核心树是 handoff 文档描述的方向，不能据此宣称本次核查的 Hermes HEAD 已移出。[Handoff 说明](https://github.com/NousResearch/hermes-plugin-holographic/blob/246d9c723a79ea07bd7455bf4b364b44269526cf/HANDOFF.md)

## 真实机制：已核实

### 事实、检索和信任

SQLite 保存事实正文、分类、标签、trust、时间、实体关联与 HRR BLOB；全文索引由触发器维护。内容精确重复返回已有事实。实体抽取是英文大写多词、引号和 aka 等规则，不是通用语言实体识别模型。[store.py](https://github.com/NousResearch/hermes-agent/blob/6f7a7991bb069db07ae74a479823ce8310f8c7e0/plugins/memory/holographic/store.py)

普通 `search` 先用 FTS5 取 `limit × 3` 个候选，再按 FTS、词集合 Jaccard、HRR 相似度重排，乘 trust；时间半衰期默认 0，即关闭。HRR 重排不能补回没有进入全文候选集的同义改写。`probe` / `related` / `reason` 可以扫描事实向量，其中 `reason` 是组合实体评分，不是 LLM 推理。`contradict` 依据共享实体与低相似度给出候选冲突，不能判定真假。[retrieval.py](https://github.com/NousResearch/hermes-agent/blob/6f7a7991bb069db07ae74a479823ce8310f8c7e0/plugins/memory/holographic/retrieval.py)

反馈 helpful 加 0.05，unhelpful 减 0.10，限制在 [0,1]。这是使用反馈权重，不能解释成已经学习到真实可信度。[反馈实现](https://github.com/NousResearch/hermes-agent/blob/6f7a7991bb069db07ae74a479823ce8310f8c7e0/plugins/memory/holographic/store.py#L205)

### HRR 是什么

HRR（Holographic Reduced Representations）是一种向量符号表示。这个实现把词经 SHA-256 确定性编码为角度向量，默认 1024 维；文本用空格切词并叠加，内容与实体角色做绑定。绑定是相位加法，解绑是相位减法，叠加是圆均值；相似度是相位差的余弦平均。它没有用训练好的语义 embedding 模型。因此“全息”不代表理解所有语义或把无限完整历史压进一个向量。[holographic.py](https://github.com/NousResearch/hermes-agent/blob/6f7a7991bb069db07ae74a479823ce8310f8c7e0/plugins/memory/holographic/holographic.py)

### 写入与生命周期

Provider 通过提示词介绍记忆工具，每轮预取最多 5 条结果；`fact_store` 提供增、查、实体探测、组合查询、冲突候选、改、删、列，`fact_feedback` 接收反馈。内置记忆镜像只处理 `add`；不能假设 replace/remove 自动同步。会话末尾自动抽取默认 `false`；开启后使用英文偏好/决定正则保存用户文本前 400 字符，不是 LLM 总结。当前代码排除压缩摘要，并从合并摘要消息中保留真实用户前缀。[provider 实现](https://github.com/NousResearch/hermes-agent/blob/6f7a7991bb069db07ae74a479823ce8310f8c7e0/plugins/memory/holographic/__init__.py)

`remove_fact` 是 SQL DELETE；`update_fact` 原地修改。Bundled schema 没有 active/archived/superseded、member_id、project_id 或来源事件外键。时间降权不等于可恢复归档。Viva story 4 的“退出活跃记忆但以后还能翻出来”仍有缺口。分类和标签也不是权限边界。[存储与删除实现](https://github.com/NousResearch/hermes-agent/blob/6f7a7991bb069db07ae74a479823ce8310f8c7e0/plugins/memory/holographic/store.py)

## 依赖与跨 harness 接入

Bundled 代码能在缺 NumPy 时退回 FTS/Jaccard，HRR 关闭；不要把这个状态当完整 HRR 已工作。独立 handoff 包要求 Python >=3.11 和 `numpy>=1.26,<3`，与 README 的“NumPy optional”表述需区分。[handoff 包配置](https://github.com/NousResearch/hermes-plugin-holographic/blob/246d9c723a79ea07bd7455bf4b364b44269526cf/pyproject.toml)

社区 standalone 包要求 Python >=3.10，核心依赖为空；NumPy、FastAPI/Uvicorn、FastMCP 为可选组。README 区分 standalone 与参考插件：后者声称四层记忆、embedding、reranker、LLM 抽取等，不能套到官方 bundled 上。[包配置](https://github.com/bysc1000/holographic-memory/blob/166ba35c5dd6973956dabee97165fc4eb0750b20/pyproject.toml)、[README](https://github.com/bysc1000/holographic-memory/blob/166ba35c5dd6973956dabee97165fc4eb0750b20/README.md)

社区仓库有 stdio MCP 脚本，但固定快照脚本实际从 Hermes 源码目录导入 `holographic.store`，并调用增强接口；它不是仅安装 standalone 包就已证明可用的官方独立服务。配置示例也需对照目标客户端当前规范验证。没有运行其 MCP server、没有安装依赖或读取用户记忆。[MCP 脚本](https://github.com/bysc1000/holographic-memory/blob/166ba35c5dd6973956dabee97165fc4eb0750b20/scripts/holographic_mcp_server.py)

## 对 Viva 的影响：设计推论，尚未实施

1. 保留 Rust 宿主、Pi 首个对话宿主、SQLite 办公室状态裁决。Holographic 自己也用 SQLite，没有冲突；办公室数据库和记忆数据库职责不同，无需共用数据库文件。
2. 优先复用用户实际使用的 Holographic 版本。检查过 bundled provider、handoff、community standalone 与 MCP 脚本；已知缺口是 Pi/其他 harness 接口、成员/项目作用域、可恢复退出和来源审计。最小适配应填这些缺口，不重写模型循环或 HRR。
3. 接口可以是本地 helper/MCP 或 Pi 扩展；Rust 通过接口调用 Python 并不意味着主程序重新改回 Python。当前未裁决具体接口，也未证明需要独立常驻服务。
4. 按成员与工作范围选择库或访问边界；共享检索需明确授权，不能把 category 当隔离。写入携带来源，归档保持可回看，反馈与读取分开。Viva 身份记录不由事实检索分数决定。

## 性能与质量证据

已核实源码具有本地无模型检索路径，因此可推断没有 embedding API 请求开销；不能由此推出比其他方案快或达到 Intel Mac 的 16 会话预算。写入会重建分类 bank；向量探测可全扫描；冲突候选是成对比较并限制最近 500 条。原始事实仍逐条持久化，向量池容量不是无限。[存储](https://github.com/NousResearch/hermes-agent/blob/6f7a7991bb069db07ae74a479823ce8310f8c7e0/plugins/memory/holographic/store.py)、[检索](https://github.com/NousResearch/hermes-agent/blob/6f7a7991bb069db07ae74a479823ce8310f8c7e0/plugins/memory/holographic/retrieval.py)

未做本机 benchmark、召回测试、16 并发、恢复或跨 harness 验收。建议用中文/英文事实、同义改写、失效事实、冲突事实与跨项目重名实体测 Recall@k/误召回，同时量 p50/p95 延迟、RSS、并发锁等待；验证缺 NumPy 降级是否可见、会话结束写入是否发生、归档是否可恢复、Pi 与第二个 harness 是否使用同一受控记忆接口。

## 网络文章：定位材料，证据等级低于固定源码

- [官方 Memory Providers 文档](https://hermes-agent.nousresearch.com/docs/user-guide/features/memory-providers)：解释 provider 与内置记忆的关系；通用生命周期描述不能自动套到每个 provider 的实现。
- [Hindsight 技术解读，2026-04-21](https://hindsight.vectorize.io/guides/2026/04/21/guide-hermes-agent-holographic-memory-technical-deep-dive)：记忆产品厂商的解释/对比材料；机制应回查源码，性能与产品结论不能视为独立验证。
- [用户第三方 provider 测试，2026-05-12](https://izzuddin8803.medium.com/i-tested-hermes-agents-third-party-memory-providers-so-you-don-t-have-to-here-are-what-i-found-94138d7b85fb)：小样本体验，提到 27 条事实、21 条命中；不能当跨机器、跨负载基准，也不能证明全量理解或稳定召回。

## 尚需确认

公开资料已足够解释方案，但尚未确认用户 Hermes 使用的是 bundled、handoff，还是社区增强版及具体 commit。不能凭同一个“Holographic”名字自动选择集成实现。下一步先由用户提供所用项目/版本信息，再验证薄适配和上述缺口；不读取个人记忆内容或凭证。
