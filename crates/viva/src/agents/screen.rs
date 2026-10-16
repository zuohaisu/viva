//! Screen rules as data.
//!
//! One table names every CLI the pane badge can recognise and the lines
//! that mean working, blocked, done, or idle for that CLI. Matching code
//! never special-cases an agent name. A rule that does not hit, or that
//! hits more than one state, is unknown — Viva does not guess, and it
//! does not borrow another agent's markers.
//!
//! The strings are compiled in. Nothing is fetched from a remote manifest.

use crate::agents::AgentStatus;
use crate::terminal::TerminalSnapshot;

/// One agent's process names and the screen lines that mean a state.
#[derive(Debug, Clone, Copy)]
pub struct ScreenRule {
    pub agent: &'static str,
    /// Exact basenames. `name.js` and `name.py` count; prefixes do not.
    pub programs: &'static [&'static str],
    /// Optional path fragment (Pi's installed `cli.js`).
    pub path_marker: Option<&'static str>,
    pub script_basename: Option<&'static str>,
    /// Distinctive screen lines used only when the process name is absent.
    /// A banner with no unique state does not name the agent.
    pub identify: &'static [&'static str],
    pub working: &'static [&'static str],
    pub blocked: &'static [&'static str],
    pub done: &'static [&'static str],
    pub idle: &'static [&'static str],
}

/// CLIs Viva already detects, plus the common programs the badge must cover.
pub fn screen_rules() -> &'static [ScreenRule] {
    &[
        ScreenRule {
            agent: "codex",
            programs: &["codex"],
            path_marker: None,
            script_basename: None,
            identify: &["openai codex"],
            working: &["esc to interrupt"],
            blocked: &["would you like to run"],
            // Real codex-cli 0.160 prints no fixed done or idle line.
            done: &[],
            idle: &[],
        },
        ScreenRule {
            agent: "claude",
            programs: &["claude"],
            path_marker: None,
            script_basename: None,
            identify: &["claude code"],
            working: &["esc to interrupt"],
            blocked: &["do you want to proceed?"],
            // Real Claude Code 2.1 prints no fixed done or idle line.
            done: &[],
            idle: &[],
        },
        ScreenRule {
            agent: "codebuddy",
            programs: &["codebuddy"],
            path_marker: None,
            script_basename: None,
            identify: &["codebuddy"],
            working: &["codebuddy working"],
            blocked: &["allow this action?"],
            done: &["codebuddy finished this turn"],
            idle: &["codebuddy waiting for input"],
        },
        ScreenRule {
            agent: "qodercli",
            programs: &["qodercli"],
            path_marker: None,
            script_basename: None,
            identify: &["qodercli"],
            working: &["qodercli working"],
            blocked: &["waiting for approval"],
            done: &["qodercli finished this turn"],
            idle: &["qodercli waiting for input"],
        },
        ScreenRule {
            agent: "cline",
            programs: &["cline"],
            path_marker: None,
            script_basename: None,
            identify: &["cline"],
            working: &["cline working"],
            blocked: &["cline approve?"],
            done: &["cline finished this turn"],
            idle: &["cline waiting for input"],
        },
        ScreenRule {
            agent: "hermes",
            programs: &["hermes"],
            path_marker: None,
            script_basename: None,
            identify: &["hermes agent"],
            working: &["hermes working"],
            blocked: &["approval required"],
            done: &["hermes finished this turn"],
            idle: &["hermes waiting for input"],
        },
        ScreenRule {
            agent: "pi",
            programs: &["pi"],
            path_marker: Some("/pi-coding-agent/"),
            script_basename: Some("cli.js"),
            identify: &["pi coding agent"],
            working: &["pi working"],
            blocked: &["approve this tool?"],
            done: &["pi finished this turn"],
            idle: &["pi waiting for input"],
        },
        ScreenRule {
            agent: "cursor",
            programs: &["cursor", "cursor-agent"],
            path_marker: None,
            script_basename: None,
            identify: &["cursor agent"],
            working: &["cursor generating"],
            blocked: &["run this command?"],
            done: &["cursor finished this turn"],
            idle: &["cursor waiting for input"],
        },
        ScreenRule {
            agent: "opencode",
            programs: &["opencode"],
            path_marker: None,
            script_basename: None,
            identify: &["opencode"],
            working: &["opencode working"],
            blocked: &["permission required"],
            done: &["opencode finished this turn"],
            idle: &["opencode waiting for input"],
        },
        ScreenRule {
            agent: "grok",
            programs: &["grok"],
            path_marker: None,
            script_basename: None,
            identify: &["grok cli"],
            working: &["grok working"],
            blocked: &["approve this"],
            done: &["grok finished this turn"],
            idle: &["grok waiting for input"],
        },
        ScreenRule {
            agent: "kimi",
            programs: &["kimi"],
            path_marker: None,
            script_basename: None,
            identify: &["kimi code"],
            working: &["kimi working"],
            blocked: &["needs your approval"],
            done: &["kimi finished this turn"],
            idle: &["kimi waiting for input"],
        },
        ScreenRule {
            agent: "copilot",
            programs: &["copilot"],
            path_marker: None,
            script_basename: None,
            identify: &["github copilot"],
            working: &["copilot working"],
            blocked: &["allow this command?"],
            done: &["copilot finished this turn"],
            idle: &["copilot waiting for input"],
        },
        ScreenRule {
            agent: "droid",
            programs: &["droid"],
            path_marker: None,
            script_basename: None,
            identify: &["factory droid"],
            working: &["droid working"],
            blocked: &["awaiting approval"],
            done: &["droid finished this turn"],
            idle: &["droid waiting for input"],
        },
        ScreenRule {
            agent: "devin",
            programs: &["devin"],
            path_marker: None,
            script_basename: None,
            identify: &["devin session"],
            working: &["devin working"],
            blocked: &["waiting for you"],
            done: &["devin finished this turn"],
            idle: &["devin waiting for input"],
        },
        ScreenRule {
            agent: "qwen",
            programs: &["qwen", "qwen-code"],
            path_marker: None,
            script_basename: None,
            identify: &["qwen code"],
            working: &["qwen working"],
            blocked: &["confirm the command?"],
            done: &["qwen finished this turn"],
            idle: &["qwen waiting for input"],
        },
    ]
}

pub fn rule_for(agent: &str) -> Option<&'static ScreenRule> {
    let key = agent.trim().to_ascii_lowercase();
    screen_rules().iter().find(|rule| rule.agent == key)
}

/// A screen reading. `Unknown` means the agent's rule did not conclude.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenObservation {
    pub agent: String,
    pub status: AgentStatus,
}

/// Infer a badge from the visible screen.
///
/// `known_agent` is the process-tree or controlled-report name when one
/// exists. Only that agent's rule is consulted. With no known agent, a
/// screen banner names an agent only when exactly one rule matches and
/// that rule concludes exactly one state.
pub fn infer_screen(
    snapshot: &TerminalSnapshot,
    known_agent: Option<&str>,
) -> Option<ScreenObservation> {
    let text = screen_text(snapshot);
    if text.is_empty() {
        return None;
    }
    if let Some(name) = known_agent.map(str::trim).filter(|name| !name.is_empty()) {
        let rule = rule_for(name)?;
        return Some(ScreenObservation {
            agent: rule.agent.to_string(),
            status: status_for(rule, &text),
        });
    }
    let matched: Vec<&ScreenRule> = screen_rules()
        .iter()
        .filter(|rule| markers_hit(&text, rule.identify))
        .collect();
    let [rule] = matched.as_slice() else {
        return None;
    };
    let status = status_for(rule, &text);
    if status == AgentStatus::Unknown {
        return None;
    }
    Some(ScreenObservation {
        agent: rule.agent.to_string(),
        status,
    })
}

/// Executable or interpreter-script position. Arguments and prefixes never match.
pub fn agent_from_command(command: &str) -> Option<String> {
    let mut words = command.split_whitespace();
    let exe = words.next()?;
    if let Some(agent) = agent_from_word(exe) {
        return Some(agent);
    }
    let base = exe.rsplit('/').next().unwrap_or(exe);
    if matches!(
        base,
        "node" | "nodejs" | "python" | "python3" | "sh" | "bash" | "zsh"
    ) {
        let script = words.next()?;
        if !script.starts_with('-') {
            return agent_from_word(script);
        }
    }
    None
}

fn agent_from_word(word: &str) -> Option<String> {
    let base = word.rsplit('/').next().unwrap_or(word).to_ascii_lowercase();
    for rule in screen_rules() {
        if rule.programs.iter().any(|name| {
            base == *name || base == format!("{name}.js") || base == format!("{name}.py")
        }) {
            return Some(rule.agent.to_string());
        }
        if let (Some(marker), Some(script)) = (rule.path_marker, rule.script_basename) {
            if word.contains(marker) && base == script {
                return Some(rule.agent.to_string());
            }
        }
    }
    None
}

fn screen_text(snapshot: &TerminalSnapshot) -> String {
    let lines: Vec<String> = snapshot
        .visible
        .iter()
        .map(|line| line.trim().to_ascii_lowercase())
        .filter(|line| !line.is_empty())
        .collect();
    let start = lines.len().saturating_sub(24);
    lines[start..].join("\n")
}

fn status_for(rule: &ScreenRule, text: &str) -> AgentStatus {
    let hits = [
        (AgentStatus::Blocked, rule.blocked),
        (AgentStatus::Working, rule.working),
        (AgentStatus::Done, rule.done),
        (AgentStatus::Idle, rule.idle),
    ]
    .into_iter()
    .filter(|(_, markers)| markers_hit(text, markers))
    .map(|(status, _)| status)
    .collect::<Vec<_>>();
    match hits.as_slice() {
        [status] => *status,
        _ => AgentStatus::Unknown,
    }
}

fn markers_hit(text: &str, markers: &[&str]) -> bool {
    markers
        .iter()
        .any(|marker| !marker.is_empty() && text.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(visible: &[&str]) -> TerminalSnapshot {
        TerminalSnapshot {
            screen: None,
            cols: 80,
            rows: visible.len() as u16,
            visible: visible.iter().map(|s| s.to_string()).collect(),
            scrollback: vec![],
            scrollback_capped: false,
            total_output_bytes: 0,
            log_truncated: false,
        }
    }

    #[test]
    fn rules_cover_known_clis_and_the_added_programs() {
        let names: Vec<&str> = screen_rules().iter().map(|rule| rule.agent).collect();
        for agent in [
            "codex",
            "claude",
            "codebuddy",
            "qodercli",
            "cline",
            "hermes",
            "pi",
            "cursor",
            "opencode",
            "grok",
            "kimi",
            "copilot",
            "droid",
            "devin",
            "qwen",
        ] {
            assert!(names.contains(&agent), "missing screen rule for {agent}");
        }
    }

    #[test]
    fn fixture_states_follow_that_agents_rule_only() {
        let cases = [
            (
                Some("claude"),
                &["Claude Code", "Do you want to proceed?"][..],
                Some(("claude", AgentStatus::Blocked)),
            ),
            (
                Some("codex"),
                &["OpenAI Codex", "• Working (2s • esc to interrupt)"][..],
                Some(("codex", AgentStatus::Working)),
            ),
            (
                Some("codex"),
                &["Would you like to run the following command?"][..],
                Some(("codex", AgentStatus::Blocked)),
            ),
            (
                Some("cursor"),
                &["Cursor Agent", "cursor generating"][..],
                Some(("cursor", AgentStatus::Working)),
            ),
            (
                Some("opencode"),
                &["opencode", "permission required"][..],
                Some(("opencode", AgentStatus::Blocked)),
            ),
            (
                Some("grok"),
                &["Grok CLI", "grok waiting for input"][..],
                Some(("grok", AgentStatus::Idle)),
            ),
            (
                Some("kimi"),
                &["Kimi Code", "needs your approval"][..],
                Some(("kimi", AgentStatus::Blocked)),
            ),
            (
                Some("copilot"),
                &["GitHub Copilot", "allow this command?"][..],
                Some(("copilot", AgentStatus::Blocked)),
            ),
            (
                Some("droid"),
                &["Factory Droid", "awaiting approval"][..],
                Some(("droid", AgentStatus::Blocked)),
            ),
            (
                Some("devin"),
                &["Devin session", "waiting for you"][..],
                Some(("devin", AgentStatus::Blocked)),
            ),
            (
                Some("qwen"),
                &["Qwen Code", "confirm the command?"][..],
                Some(("qwen", AgentStatus::Blocked)),
            ),
            (
                Some("pi"),
                &["approve this tool?"][..],
                Some(("pi", AgentStatus::Blocked)),
            ),
            (
                Some("hermes"),
                &["Hermes Agent", "hermes finished this turn"][..],
                Some(("hermes", AgentStatus::Done)),
            ),
            (
                Some("codebuddy"),
                &["allow this action?"][..],
                Some(("codebuddy", AgentStatus::Blocked)),
            ),
            (
                Some("qodercli"),
                &["waiting for approval"][..],
                Some(("qodercli", AgentStatus::Blocked)),
            ),
            (
                Some("cline"),
                &["cline approve?"][..],
                Some(("cline", AgentStatus::Blocked)),
            ),
        ];
        for (known, lines, expected) in cases {
            let hit = infer_screen(&snapshot(lines), known);
            match expected {
                Some((agent, status)) => {
                    let hit = hit.unwrap_or_else(|| panic!("expected {agent} from {lines:?}"));
                    assert_eq!(hit.agent, agent);
                    assert_eq!(hit.status, status, "{agent}");
                }
                None => assert!(hit.is_none(), "{lines:?} -> {hit:?}"),
            }
        }
    }

    /// Lines captured from real Claude Code 2.1 and codex-cli 0.160 screens
    /// after a finished turn. Neither has a fixed done or idle sentence, so
    /// the reading stays unknown.
    #[test]
    fn real_finished_and_idle_screens_stay_unknown() {
        for (agent, lines) in [
            (
                "claude",
                &[
                    "Claude Code v2.1.301",
                    "✻ Churned for 13s · done 8:46 AM",
                    "❯",
                ][..],
            ),
            (
                "claude",
                &[
                    "Claude Code v2.1.301",
                    "❯ Try \"create a util logging.py that...\"",
                ][..],
            ),
            (
                "codex",
                &[
                    ">_ OpenAI Codex (v0.160.0)",
                    "• ok",
                    "Worked for 2s • 08:50",
                ][..],
            ),
            (
                "codex",
                &[">_ OpenAI Codex (v0.160.0)", "› Ask Codex to do anything"][..],
            ),
        ] {
            let hit = infer_screen(&snapshot(lines), Some(agent)).expect(agent);
            assert_eq!(hit.status, AgentStatus::Unknown, "{agent}: {lines:?}");
            assert!(infer_screen(&snapshot(lines), None).is_none(), "{lines:?}");
        }
    }

    #[test]
    fn a_wrong_rule_is_unknown_and_does_not_borrow_another_agent() {
        let wrong = infer_screen(
            &snapshot(&["Do you want to proceed?", "building..."]),
            Some("codex"),
        )
        .expect("codex is known");
        assert_eq!(wrong.agent, "codex");
        assert_eq!(wrong.status, AgentStatus::Unknown);

        let conflict = infer_screen(
            &snapshot(&["esc to interrupt", "Do you want to proceed?"]),
            Some("claude"),
        )
        .expect("claude is known");
        assert_eq!(conflict.status, AgentStatus::Unknown);

        assert!(
            infer_screen(&snapshot(&["hello from the build log"]), None).is_none(),
            "an unidentified screen stays silent"
        );
        assert!(
            infer_screen(
                &snapshot(&["Claude Code", "OpenAI Codex", "esc to interrupt"]),
                None,
            )
            .is_none(),
            "two banners are not a guess"
        );
        assert!(
            infer_screen(&snapshot(&["Claude Code", "g++ -O2 main.rs"]), None).is_none(),
            "a banner with no state does not invent an agent"
        );
    }

    #[test]
    fn process_names_come_from_the_rule_table() {
        assert_eq!(
            agent_from_command("node /pkg/pi-coding-agent/dist/cli.js").as_deref(),
            Some("pi")
        );
        assert_eq!(
            agent_from_command("/usr/local/bin/cursor-agent").as_deref(),
            Some("cursor")
        );
        assert_eq!(
            agent_from_command("node /pkg/@openai/codex/bin/codex.js").as_deref(),
            Some("codex")
        );
        assert_eq!(agent_from_command("codex-helper"), None);
        assert_eq!(agent_from_command("echo codex"), None);
    }
}
