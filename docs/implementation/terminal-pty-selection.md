# V05 Terminal / PTY Reuse Selection

Status: **Delivered with V05 (issue #14)** · 2026-09-28 · Lane E

Issue #14's 2026-09-28 supplement requires a recorded reuse check against
the Orca fixed-source audit before choosing a PTY approach. This document
is that record.

## Checked capabilities

- **Orca TerminalHost / PTY slice** (`docs/research/orca-reuse-audit-2026-09-28.md`):
  the audit found no plain-Node `orcad` build entry without Electron, and
  the TerminalHost slice is not published as an independent library —
  extracting it means carrying Orca's runner/cache/WSL coupling. Reuse
  would also import Orca's detach/disconnect-then-keep-running semantics,
  which Viva must not keep (owned terminals stop with the Viva host).
- **portable-pty 0.9.0** (wezterm's pty layer, maintained, MIT): owns the
  pty/session discipline (setsid + controlling terminal on Unix), giving
  each session a real process group without hand-rolled tty ioctls.
- **vt100 0.16.2** (MIT): parses raw output into a bounded grid + ring
  scrollback; byte-exact ANSI fidelity is preserved separately in the
  disk log, so nothing fakes the transcript.

## Decision

Use portable-pty (session/process-group discipline) + vt100 (grid
emulation). Viva's own `terminal::session` owns lifecycle: SIGTERM →
timeout → SIGKILL to the process group with reap confirmation, bounded
memory, and byte-level redacted disk logs. No Orca code, binary, CLI, or
private metadata is used; no Orca.app dependency exists.

## Licenses (transitive set of the new dependencies)

| Crate | Version | License |
| --- | --- | --- |
| portable-pty | 0.9.0 | MIT |
| vt100 | 0.16.2 | MIT |
| ratatui | 0.30.2 | MIT |
| crossterm | 0.29.0 | MIT |
| libc | 0.2.189 | MIT OR Apache-2.0 |

## Pending (not claimed here)

- Intel Mac acceptance stays with V12; this lane validated on macOS arm64
  and Linux CI runs the same suite.
- Real Pi composition (V09) and 16-Agent soak (V12) are separate gates.
