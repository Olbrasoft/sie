//! Synthetic process/tmux/window metadata only; no user's desktop is inspected.
use sie::agent_origin::*;
use std::path::PathBuf;

fn question() -> Question {
    Question {
        agent: "codex".into(),
        session_id: "session-1".into(),
        cwd: "/project".into(),
        text: "Pokračovat?".into(),
    }
}
fn fixture() -> Snapshot {
    let mut s = Snapshot::default();
    for (pid, parent, name, cwd) in [
        (10, 1, "Alacritty", None),
        (11, 10, "bash", None),
        (12, 11, "tmux", None),
        (200, 1, "tmux", None),
        (30, 200, "bash", None),
        (31, 30, "codex", Some(PathBuf::from("/project"))),
    ] {
        s.processes.insert(
            pid,
            Process {
                pid,
                parent,
                started: u64::from(pid) * 100,
                name: name.into(),
                cwd,
                daemon: false,
            },
        );
    }
    s.panes.push(Pane {
        id: "%1".into(),
        pid: 30,
        session: "codex-project".into(),
        window: "@1".into(),
        cwd: "/project".into(),
    });
    s.clients.push(Client {
        pid: 12,
        tty: "/dev/pts/8".into(),
        session: "codex-project".into(),
    });
    s.windows.push(
        serde_json::from_value(
            serde_json::json!({"id":100,"pid":10,"wm_class":"Alacritty","workspace":3}),
        )
        .unwrap(),
    );
    s
}
fn add_other(s: &mut Snapshot) {
    for (pid, parent, name, cwd) in [
        (50, 1, "Alacritty", None),
        (51, 50, "bash", None),
        (52, 51, "tmux", None),
        (40, 200, "bash", None),
        (41, 40, "codex", Some(PathBuf::from("/project"))),
    ] {
        s.processes.insert(
            pid,
            Process {
                pid,
                parent,
                started: u64::from(pid) * 100,
                name: name.into(),
                cwd,
                daemon: false,
            },
        );
    }
    s.panes.push(Pane {
        id: "%2".into(),
        pid: 40,
        session: "codex-project-other".into(),
        window: "@2".into(),
        cwd: "/project".into(),
    });
    s.clients.push(Client {
        pid: 52,
        tty: "/dev/pts/9".into(),
        session: "codex-project-other".into(),
    });
    s.windows.push(
        serde_json::from_value(
            serde_json::json!({"id":200,"pid":50,"wm_class":"Alacritty","workspace":5}),
        )
        .unwrap(),
    );
}

#[test]
fn attached_agent_routes_via_its_tmux_client_not_detached_server_ancestry() {
    let s = fixture();
    let origin = resolve(&s, &question()).unwrap();
    assert_eq!(origin.window, 100);
    assert_eq!(origin.agent_pid, 31);
    assert_eq!(origin.client_tty, "/dev/pts/8");
    assert_eq!(origin.pane, "%1");
    assert_eq!(origin.workspace(&s), Some(3));
    assert!(origin.still_valid(&s, &question()));
}

#[test]
fn cwd_fallback_rejects_ambiguous_sessions_and_shared_gui_windows() {
    let mut s = fixture();
    add_other(&mut s);
    assert!(resolve(&s, &question()).is_none());
    s.indexed_pids.insert(31);
    assert_eq!(resolve(&s, &question()).unwrap().window, 100);
    let mut shared = fixture();
    let mut window = shared.windows[0].clone();
    window.id = 101;
    shared.windows.push(window);
    assert!(resolve(&shared, &question()).is_none());
}

#[test]
fn detached_agents_and_shared_codex_daemons_are_not_guessed_as_origins() {
    let mut s = fixture();
    s.clients.clear();
    assert!(resolve(&s, &question()).is_none());
    let mut s = fixture();
    s.processes.get_mut(&31).unwrap().daemon = true;
    s.indexed_pids.insert(31);
    assert!(resolve(&s, &question()).is_none());
    let mut s = fixture();
    s.processes.get_mut(&31).unwrap().cwd = Some("/unrelated".into());
    assert!(resolve(&s, &question()).is_none());
}

#[test]
fn moving_a_window_is_allowed_but_recycled_closed_or_replaced_targets_are_rejected() {
    let s = fixture();
    let origin = resolve(&s, &question()).unwrap();
    let mut moved = s.clone();
    moved.windows[0].workspace = Some(4);
    assert!(origin.still_valid(&moved, &question()));
    assert_eq!(origin.workspace(&moved), Some(4));
    for pid in [10, 12, 30, 31] {
        let mut recycled = s.clone();
        recycled.processes.get_mut(&pid).unwrap().started += 1;
        assert!(!origin.still_valid(&recycled, &question()));
    }
    let mut closed = s.clone();
    closed.windows.clear();
    assert!(!origin.still_valid(&closed, &question()));
    let mut changed = s.clone();
    changed.panes[0].session = "other".into();
    assert!(!origin.still_valid(&changed, &question()));
    let mut changed = s.clone();
    changed.clients[0].tty = "/dev/pts/9".into();
    assert!(!origin.still_valid(&changed, &question()));
}

#[test]
fn malformed_tmux_metadata_and_unbounded_question_inputs_are_rejected() {
    assert_eq!(parse_panes("%1\t30\tagent\t@1\t/project\n").len(), 1);
    assert!(parse_panes("1\t30\tagent\t@1\t/project\n").is_empty());
    assert!(parse_panes("%1\t30\tagent\t@1\trelative\n").is_empty());
    assert_eq!(parse_clients("12\t/dev/pts/8\tagent\n").len(), 1);
    assert!(parse_clients("12\tbogus\tagent\n").is_empty());
    assert!(question().validate().is_ok());
    let mut q = question();
    q.session_id = "session;command".into();
    assert!(q.validate().is_err());
    let mut q = question();
    q.cwd = "relative".into();
    assert!(q.validate().is_err());
    let mut q = question();
    q.text = "x".repeat(16385);
    assert!(q.validate().is_err());
}
