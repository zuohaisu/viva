use viva::agents::{
    AgentStatus, AgentStatusRecord, CapabilityLevel, ProcessTable, StatusSource,
    agent_from_command, capability_matrix, profile_for, report_stale,
};
#[test]
fn detection_matches_executables_and_scripts_not_shell_text_or_tests() {
    for text in [
        "echo codex",
        "pytest tests/pi.py",
        "/bin/sh -c pi",
        "codex-helper",
        "cargo test claude",
        "node test.js pi",
    ] {
        assert_eq!(agent_from_command(text), None, "{text}");
    }
    for (text, agent) in [
        ("/bin/codex --help", "codex"),
        ("/bin/sh /tmp/codex", "codex"),
        ("node /pkg/@openai/codex/bin/codex.js", "codex"),
        ("node /pkg/pi-coding-agent/dist/cli.js", "pi"),
    ] {
        assert_eq!(agent_from_command(text).as_deref(), Some(agent));
    }
}
#[test]
fn process_instances_and_exits_are_separate_even_for_the_same_tool() {
    let table = ProcessTable::parse(
        "100 1 Wed Oct 7 00:00:00 2026 /bin/zsh\n101 100 Wed Oct 7 00:01:00 2026 codex\n200 1 Wed Oct 7 00:02:00 2026 /bin/zsh\n201 200 Wed Oct 7 00:03:00 2026 codex",
    );
    let a = table.identify(Some(100)).unwrap();
    let b = table.identify(Some(200)).unwrap();
    assert_eq!(a.agent, b.agent);
    assert_ne!(a.pid, b.pid);
    assert_eq!(
        ProcessTable::parse("100 1 Wed Oct 7 00:00:00 2026 /bin/zsh").identify(Some(100)),
        None
    );
}
#[test]
fn capability_matrix_marks_unknown_and_unverified_honestly() {
    assert_eq!(capability_matrix().len(), 7);
    let pi = profile_for("pi").expect("pi");
    assert_eq!(pi.controlled_report, CapabilityLevel::Verified);
    let cline = profile_for("cline").expect("cline");
    assert_eq!(cline.controlled_report, CapabilityLevel::Unknown);
    let codex = profile_for("codex").expect("codex");
    assert_eq!(codex.controlled_report, CapabilityLevel::Unverified);
}

#[test]
fn stale_report_keeps_source_and_cannot_become_completion() {
    let r = AgentStatusRecord {
        terminal_id: "t".into(),
        agent: "pi".into(),
        status: AgentStatus::Done,
        source: StatusSource::ControlledReport,
        detail: "history".into(),
        updated_at: "2000-01-01T00:00:00Z".into(),
    };
    assert!(report_stale(&r));
    assert_eq!(r.source, StatusSource::ControlledReport);
}
