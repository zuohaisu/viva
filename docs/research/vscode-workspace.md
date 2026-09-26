# Research — VS Code Workspace 的概念模型

Status: research note · Date: 2026-09-27 · Sources: code.visualstudio.com 官方文档（见文末）

本文只回答一个问题：**VS Code 用 "Workspace" 解决什么问题，Viva 应该吸收什么。**

## 1. VS Code 的 Workspace 模型

- **Workspace = 一个窗口中打开的一个或多个 folder 的集合**，是 "a project that has extra VS Code knowledge and capabilities"。VS Code 刻意**没有** Visual Studio 那样的 "project/solution" 概念——Workspace 就是长期工作的单位。
- 两种形态：
  - **Single-folder workspace**（隐式）：打开任意文件夹即隐式成立，配置放在 `.vscode/`；
  - **Multi-root workspace**（显式）：由 `.code-workspace` JSON 文件定义，内容为 `folders[]`（支持相对路径，可携带）、`settings`、`launch`、`tasks`、`extensions`（推荐扩展）。
- **Workspace 携带什么**：folder-scoped settings、task/launch 配置、可恢复的 UI state（打开的文件、编辑器布局、terminal 会话）、per-workspace 扩展启停。
- **Settings 层级**：Default → User → Remote → Workspace → Folder（高层覆盖低层；对象按键合并；部分安全类设置不允许 workspace scope）。这使"每个项目有自己的一套行为"成为可能，且不污染机器和其他项目。
- **State 的两分法**（最重要的设计洞察）：
  - **Configuration as files**：`settings.json` / `tasks.json` / `.code-workspace` 声明式、可提交、可携带；
  - **State as database**：打开的编辑器、布局、terminal 会话、`workspaceState`（扩展 k/v 存储）存在 user data 目录的 `workspaceStorage/<workspace-id>/`，按 workspace 身份键控，重开时 rehydrate。
  - 官方 API 契约原话：`workspaceState` — "VS Code manages the storage and will restore it when the same workspace is opened again"；与之相对的 `globalState` 是跨 workspace 的。
- **生命周期**：open / add folder / remove folder / Save Workspace As（untitled → 有持久身份）；`Open Recent` 列表的条目是 **workspace 而不是 window**；`window.restoreWindows` 决定重开时恢复哪些。**Window 是一次性的，Workspace 是持久的。**
- **与 git 的关系**：git 是 per-folder 的；multi-root = 一个窗口聚合 N 个 repo，Repositories view 统一管理，但操作前必须确认 repo 和 branch。

## 2. 为什么 Workspace 是好的长期工作单位（设计要点）

1. **一个身份，分层配置**——workspace 是 defaults / user 偏好 / 项目配置的汇合点。
2. **配置进文件，状态进数据库**——可版本化的声明 + 可恢复的隐式状态，这让"回到工作"的感觉成立。
3. **用组合扩展，不用层级**——multi-root 聚合相关 repo 或切出 scenario-scoped 视图，而不发明 build-system 级的 "solution"。
4. **跨会话的稳定身份**——recent list 和 restore 都把 workspace 当作 "the thing I'm working on" 的持久句柄。

## 3. Viva 吸收什么 / 拒绝什么

| 维度 | Viva 吸收 | Viva 拒绝 / 不做 |
| --- | --- | --- |
| 身份 | Workspace 是长期工作的持久句柄；`viva` 的 recent list 是 workspace 列表 | 无 |
| 两分法 | **Project context 进文件**（尽量放 repo 内，任何 worker 可读）；**工作状态进 Viva 本地存储**，按 workspace 键控，重开恢复 | 不把隐式状态塞进 repo |
| 分层配置 | workspace 级配置覆盖 resident 默认；project rules（类似 AGENTS.md）属于 workspace | 不做 machine/user/remote 多机层级 |
| 多 repo | 一个 Workspace 可含多个 repository（VicTrader 前后端分仓是真实需求） | 不做 VS Code 式 editor 内聚合 |
| Restore | 重开 workspace 恢复：worktree 列表、活着的 worker session、上次 episode、未完成 intentions | restore ≠ recall，时间轴部分见 `docs/architecture/temporal-model.md` |
| 边界 | Workspace 管 project context，不管编辑 | file tree / editor / debugger / extension market——编辑器交给 VS Code 本尊（见 `docs/product/product-model.md` Q9） |

## 4. 对 Viva 的直接结论

- Viva Workspace 不是 "一个 repo path"，而是 **project 的长期容器**：repositories + project context（文件化）+ worktrees 注册表 + tasks + 工作状态（本地库）+ 历史索引。
- "打开 VS Code" 的对象是 folder/repo；"进入 Viva" 的对象是 Workspace。二者可以是同一物理目录，但**生命周期与身份不同**：VS Code 窗口随开随关，Viva Workspace 跨月跨年。

## Sources

- https://code.visualstudio.com/docs/editor/workspaces
- https://code.visualstudio.com/docs/editor/multi-root-workspaces
- https://code.visualstudio.com/docs/getstarted/settings
- https://code.visualstudio.com/docs/debugtest/tasks
- https://code.visualstudio.com/docs/sourcecontrol/repos-remotes
- https://code.visualstudio.com/api/extension-capabilities/common-capabilities （workspaceState/globalState 契约）
