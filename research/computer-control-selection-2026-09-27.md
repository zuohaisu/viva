# 计算机控制对 Viva 选型的影响

日期：2026-09-27。一手 API/绑定文档核查；未实现或操控机器。只讨论既定 TUI、16 Agent 与未来系统控制需求，不设计新的通用平台。

## 结论

**Rust 作为受控执行核心的理由增强，但它不自动提高模型决策质量、任务完成速度或操作权限。** 可让 Rust 核心负责资源、动作校验、进程与桌面访问协调，复用现成 Agent 与浏览器能力。Pi 已核实的合帧、缓存、输出分流是实际机制，用户感到更快是有价值的工作负载证据；将其归因于语言并推广到 Viva 尚缺同任务测量。

## 平台事实

- macOS 的 `AXUIElement`/Accessibility 与 `CGEvent` 是原生接口；`CGEventPost` 把事件加入 Quartz event stream。Rust 可经 FFI/现成绑定调用，并非必须用 Swift。`objc2-application-services` 记录了 AX 绑定，但当前部分 AX 符号标记 deprecated，故此处只证实可接入，不定具体 crate/API 版本；实现前要核实对应系统支持与弃用原因。[Apple CGEventPost](https://developer.apple.com/documentation/coregraphics/cgevent/post%28tap%3A%29?language=objc)、[Rust ApplicationServices 绑定](https://docs.rs/objc2-application-services/latest/objc2_application_services/)
- 截屏/流式捕获可以复用 ScreenCaptureKit，Rust 已有 `objc2-screen-capture-kit` 绑定；也可以保留小型 Swift/ObjC helper。两条路最终使用相同 OS 框架，不能未经 profiling 声称纯 Rust 截屏更快。[Apple ScreenCaptureKit](https://developer.apple.com/documentation/screencapturekit?changes=_5__8)、[Rust 框架绑定](https://docs.rs/objc2-screen-capture-kit/latest/objc2_screen_capture_kit/)
- Accessibility 信任与 Screen Recording 许可由 macOS 判断。`AXIsProcessTrustedWithOptions` 查询可信状态，异步提示不会改变当次返回值。语言不能绕过授权或凭类型安全阻止合法 API 误点；动作权限、目标校验和用户审批是产品政策。[Apple Accessibility 信任检查](https://developer.apple.com/documentation/applicationservices/1459186-axisprocesstrustedwithoptions?language=objc)、[Apple 屏幕捕获示例](https://developer.apple.com/documentation/screencapturekit/capturing-screen-content-in-macos?language=objc)
- Windows 可复用 Microsoft `windows-rs` 的 COM/Win32 bindings，官方生成文档含 `Win32::UI::Accessibility`。Microsoft UI Automation 的 client API 可读取和操作其他应用暴露的控件。首版仅保留平台 adapter 边界，无需现在实现 Windows。[windows-rs](https://github.com/microsoft/windows-rs)、[Accessibility 绑定](https://microsoft.github.io/windows-docs-rs/doc/windows/Win32/UI/Accessibility/index.html)、[Microsoft UI Automation](https://learn.microsoft.com/en-us/windows/win32/winauto/entry-uiautocore-overview)
- 浏览器操作先复用 Playwright；其 BrowserContext 隔离 cookies/storage，并可在单个 browser 中创建多个 context。Rust 主控可以通过 adapter 调用现成实现，无须重写整个浏览器自动化；context 隔离不是 OS 安全沙箱，也不承诺 16 个页面没有内存负担。[Playwright isolation](https://playwright.dev/docs/browser-contexts)

## 16 并发的关键限制（工程推论）

16 个 Agent 可以同时推理、调用彼此隔离的 API、执行受控命令或使用独立浏览器 context。**不能让 16 个 Agent 同时争抢一台电脑的前台焦点、全局鼠标与键盘。** 全局事件进入共享桌面；即使每次动作的 API 调用线程安全，不同任务的“定位—点击—输入—验证”仍会相互破坏。

因此，对共享前台桌面应串行分配操作权，保护整个需要稳定焦点的动作序列；在执行前确认目标窗口/元素仍匹配，结束后验证结果，允许用户输入打断。可以定向操作 AX/UIA 元素的动作应优先于坐标点击，但仍需按目标应用/资源处理冲突。需要真正并行的全桌面操作时，须有隔离会话/机器；不能由语言选择解决。

## 最小选型含义

Rust + Tokio + Ratatui/Crossterm 仍是资源与原生控制优先的合理选择。复用 Pi 的 Agent 设计或直接复用其模块是独立选择，不强制整个系统都用 TypeScript，也不强制为了 Rust 全部重写。语言无关的硬要求是：有界数据流、真实退出确认、明确 OS 权限、动作目标验证、审计证据和共享桌面的互斥操作权。

最终速度应测同模型同任务下成功完成的墙钟时间，同时记录内存、重试与错误动作；原生内核的低开销不是整体最快的证明。
