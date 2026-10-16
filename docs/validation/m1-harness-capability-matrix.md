# M1 harness capability matrix (S6)

> Historical M1 record. The single-key walkthrough below (`p`, `o`, `d`, `h`, `w`, bare `q`) was retired in 0.3.8; current keys follow herdr — see [`0.3.8-herdr-capability-matrix.md`](0.3.8-herdr-capability-matrix.md).

Machine-readable source: `viva harness matrix` (JSON).

Dimensions (M1 exit criteria):

| Dimension | Meaning |
| --- | --- |
| launch | Viva can start the harness with explicit argv + cwd |
| interactive | PTY keyboard/session in a workbench pane |
| status_report | Controlled agent status reaches Viva (extension or hook) |
| handoff | Workbench `h` or Pi `viva_handoff` / cross-agent brief |
| native_resume | Harness-owned session resume (not Viva server resume) |
| fork | Native conversation fork inside the harness |

Evidence labels: `tested`, `unsupported`, `unknown` — never upgraded without real checks. A local `program --version` success is recorded in row notes only (install hint), not as `tested`. Bare agent CLIs stay `unknown` until this repo has real harness checks for that tool (PTY/handoff tests today use `/bin/sh` stand-ins).

## Full-chain walkthrough (daily path)

1. **Workspace / project** — `viva open`, top bar; S1 (#74).
2. **Task** — create/list in workbench; `w` creates task worktree.
3. **Agent** — `p` launches Pi with member/task env; `o` opens user shell.
4. **Diff / PR** — `d` diff; PR state on worktree rows (gh probe).
5. **Handoff** — `h` picks a matrix-listed agent; brief via bracketed paste.
6. **Recovery** — `q` detach; server keeps terminals; attach restores view.

Gaps tracked on GitHub [#27](https://github.com/zuohaisu/viva/issues/27) and [#19](https://github.com/zuohaisu/viva/issues/19).
