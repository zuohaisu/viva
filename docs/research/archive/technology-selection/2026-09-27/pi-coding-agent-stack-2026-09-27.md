> **Status: Historical / non-authoritative.** 这是裁决前的研究快照，含阶段性或已失效判断；现行技术结论只见 [ADR 0011](../../../../decisions/0011-rust-host-and-tui.md)。下方原文保留用于追溯，不代表当前实现或当前推荐。

# Pi 的资源相关设计核查

日期：2026-09-27。检查 canonical `earendil-works/pi` 的 `main`；所读 package manifest 标记 0.87.1。未安装、运行 Agent 或调用模型。这里是源代码证据，不是 Intel Mac 性能验收。

## 实际栈

Pi TUI 是 TypeScript 自有 `pi-tui`，不是 React/Ink。`tsc` 构建，Node 要求 `>=22.19.0`；TUI manifest 只有 `get-east-asian-width` 与 `marked` 两个运行依赖，并分发 macOS Objective-C、Linux/Windows C 原生辅助代码。不能把 TUI 两个依赖误说成整个 Agent 仅有两个依赖。[TUI package manifest](https://github.com/earendil-works/pi/blob/main/packages/tui/package.json)、[Agent package manifest](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/package.json)

## 能解释响应与开销的机制

- **合并与节流 UI 更新。** `TuiBase.requestRender` 对已有待渲染请求直接返回；普通更新最小间隔 16 ms。键盘事件使用 immediate 路径，抢占已调度的节流帧。源码称其为 differential rendering；这些机制可以减少重复绘制，不能据此声称实测低于其他语言。[TUI 源码](https://github.com/earendil-works/pi/blob/main/packages/tui/src/tui.ts)
- **缓存不变内容。** Markdown 组件按文本与宽度缓存输出；变更调用 invalidate。Bash 折叠显示默认 5 个 visual lines，按宽度缓存，流式输出更新以 100 ms 节流，避免每个输出 chunk 都重建预览。[Markdown 源码](https://github.com/earendil-works/pi/blob/main/packages/tui/src/components/markdown.ts)、[Bash renderer](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/src/core/tools/renderers/bash.ts)、[Bash 工具](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/src/core/tools/bash.ts)
- **展示与完整输出分离。** 工具输出默认上限是 2,000 行或 50 KiB，先达到者生效。`OutputAccumulator` 保留 rolling tail、流式 UTF-8 解码；超限后保存原始输出到临时文件并清掉此前 raw chunks。这限制了展示与模型上下文的单次输出，不代表整个会话、所有扩展、磁盘队列都受同一上限。[截断工具](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/src/core/tools/truncate.ts)、[OutputAccumulator](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/src/core/tools/output-accumulator.ts)
- **仍有资源边界要核验。** 上述 accumulator 的 `WriteStream.write(data)` 没有利用返回值等待 drain；源码中的 rolling-tail 上限不等于磁盘缓慢或大突发输出时进程总内存的硬上限。必须在目标工作负载测试背压，而不能只引用其“bounded memory”注释。[OutputAccumulator](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/src/core/tools/output-accumulator.ts)
- **不能归因于全面 lazy startup。** 当前 `main.ts` 有大量静态导入，未找到其入口普遍按需动态加载的证据。Bash renderer 确实与执行/schema 模块分开，让纯显示消费者不用加载执行路径；这只是局部模块隔离。[入口源码](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/src/main.ts)、[Bash renderer](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/src/core/tools/renderers/bash.ts)

## Benchmark 的证据等级

不能说完全没有性能数据：官方仓库的 [issue #7739](https://github.com/earendil-works/pi/issues/7739) 收录跨项目启动/内存对照以及低端机器扩展加载测量。但 issue 正文引用竞争项目的历史 benchmark，评论是用户在 Linux/Windows 的特定版本和扩展配置测量；不是 Pi maintainers 对当前 main、Intel Mac、16 并发的官方验收。它还提醒扩展加载可能主导启动开销，不能把用户感到流畅推广为所有安装配置都快。

本次没有获得足以给 Rust/Go/TS/Python 排名的同机同负载数据，也未对用户实际使用的 Pi 版本做 profiling。

作者的 [2025-11-30 原文](https://mariozechner.at/posts/2025-11-30-pi-coding-agent/) 说明了小 harness、可见上下文、少工具与分层 TUI 设计，并给出历史 Terminal-Bench 测试。这可以支持“harness 设计值得研究”，不能证明当前 main 在 Intel Mac 上的 16 并发内存或真实任务墙钟时间优于另一语言。

## 对 Viva 的结论

Pi 提供了实质反例：TypeScript 不是终端产品必然笨重的原因，React/Ink 也不是 TypeScript TUI 的必选项。可以复用轻量 TUI，或在任意语言实现其合帧、差异绘制、缓存、输出落盘与有界窗口机制。

选型不能只依据“Rust 无 GC”或“Pi 用起来快”。若资源优先，应对候选原型使用相同的 16 路数据源、输出量、可见窗口、日志保留和 Agent 拓扑，分别测 Controller 本身和子进程树。若编程复杂度不进入用户目标函数，应将其从比较标准移除，保留资源表现、安全边界和可验证性。

用户补充实际任务完成速度最重要且 Pi 体验明显更快后，**优先核查复用 TypeScript 的 `pi-tui` / `pi-agent-core`** 比直接自建 Rust harness 更符合 reuse-first。此处理由是复用已存在的执行语义与速度相关设计，不是迁移成本或编程复杂度。Rust 继续作为资源与系统约束候选，不能未经同负载验证就授予整体速度优势。模型、推理预算、提示上下文、工具调用与并发策略均会影响完成速度；TUI 重绘效率只覆盖其中一部分。
