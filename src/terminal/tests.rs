use super::*;
struct PrivateTmux {
    directory: tempfile::TempDir,
}
impl PrivateTmux {
    fn socket(&self) -> std::path::PathBuf {
        self.directory.path().join("socket")
    }
}
impl Drop for PrivateTmux {
    fn drop(&mut self) {
        let _ = std::process::Command::new("tmux")
            .arg("-S")
            .arg(self.socket())
            .arg("kill-server")
            .output();
    }
}

#[tokio::test]
async fn private_tmux_delivers_unicode_without_enter_and_suppresses_canceled_or_refocused_output() {
    let server = PrivateTmux {
        directory: tempfile::tempdir().unwrap(),
    };
    let socket = server.socket();
    let socket = Some(socket.as_path());
    query_on(
        socket,
        &["-f", "/dev/null", "new-session", "-d", "-s", "probe", "cat"],
    )
    .await
    .unwrap();
    query_on(socket, &["new-window", "-d", "-t", "probe", "cat"])
        .await
        .unwrap();
    query_on(socket, &["copy-mode", "-t", "%0"]).await.unwrap();
    query_on(socket, &["copy-mode", "-t", "%1"]).await.unwrap();
    let phrase = "Příliš žluťoučký kůň úpěl ďábelské ódy.";
    paste_checked(
        socket,
        "%0",
        phrase,
        DeliveryMode::PasteOnly,
        Arc::new(AtomicBool::new(false)),
        || async { Ok(()) },
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(80)).await;
    let captured = query_on(socket, &["capture-pane", "-p", "-t", "%0"])
        .await
        .unwrap();
    assert!(captured.contains(phrase));
    // cat's canonical input has echoed the phrase but received no Enter.
    assert_eq!(captured.trim_end(), phrase);
    assert_eq!(
        query_on(
            socket,
            &["display-message", "-p", "-t", "%0", "#{pane_in_mode}"]
        )
        .await
        .unwrap()
        .trim(),
        "0"
    );
    assert_eq!(
        query_on(
            socket,
            &["display-message", "-p", "-t", "%1", "#{pane_in_mode}"]
        )
        .await
        .unwrap()
        .trim(),
        "1"
    );
    assert!(
        !query_on(socket, &["capture-pane", "-p", "-t", "%1"])
            .await
            .unwrap()
            .contains(phrase)
    );
    assert!(
        paste_checked(
            socket,
            "%0",
            "CANCELED",
            DeliveryMode::PasteOnly,
            Arc::new(AtomicBool::new(true)),
            || async { Ok(()) }
        )
        .await
        .is_err()
    );
    assert!(
        paste_checked(
            socket,
            "%0",
            "WRONG TARGET",
            DeliveryMode::PasteOnly,
            Arc::new(AtomicBool::new(false)),
            || async { anyhow::bail!("focus changed") }
        )
        .await
        .is_err()
    );
    let after = query_on(socket, &["capture-pane", "-p", "-t", "%0"])
        .await
        .unwrap();
    assert_eq!(after, captured);
    assert!(
        !query_on(socket, &["list-buffers", "-F", "#{buffer_name}"])
            .await
            .unwrap()
            .contains("sie-dictation-")
    );
}

#[tokio::test]
async fn private_tmux_submits_once_and_rechecks_cancel_and_target_after_paste() {
    for scenario in [
        "submit",
        "paste-only",
        "deferred-submit",
        "deferred-cancel",
        "deferred-refocus",
        "refocus-after",
        "cancel-after",
        "empty",
        "cancel-before",
    ] {
        let server = PrivateTmux {
            directory: tempfile::tempdir().unwrap(),
        };
        let socket = server.socket();
        let socket = Some(socket.as_path());
        query_on(
            socket,
            &[
                "-f",
                "/dev/null",
                "new-session",
                "-d",
                "-s",
                "probe",
                "stty -echo; printf 'READY\\n'; cat -n",
            ],
        )
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if query_on(socket, &["capture-pane", "-p", "-t", "%0"])
                    .await
                    .unwrap()
                    .contains("READY")
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let cancelled = Arc::new(AtomicBool::new(scenario == "cancel-before"));
        let text = if scenario == "empty" {
            " \n\t"
        } else {
            "Příliš žluťoučký kůň."
        };
        let mode = if scenario == "paste-only" || scenario.starts_with("deferred-") {
            DeliveryMode::PasteOnly
        } else {
            DeliveryMode::Submit
        };
        let mut checks = 0;
        let mut result = paste_checked(socket, "%0", text, mode, cancelled.clone(), || {
            checks += 1;
            let check = checks;
            let cancelled = cancelled.clone();
            async move {
                if check == 2 {
                    ensure!(scenario != "refocus-after", "Target pane changed");
                    if scenario == "cancel-after" {
                        cancelled.store(true, Ordering::SeqCst);
                    }
                }
                Ok(())
            }
        })
        .await;
        if scenario.starts_with("deferred-") {
            assert!(result.is_ok());
            cancelled.store(scenario == "deferred-cancel", Ordering::SeqCst);
            result = submit_checked(socket, "%0", &cancelled, || async {
                ensure!(scenario != "deferred-refocus", "Target pane changed");
                Ok(())
            })
            .await;
        }
        assert_eq!(
            result.is_ok(),
            ["submit", "paste-only", "empty", "deferred-submit"].contains(&scenario),
            "{scenario}"
        );
        tokio::time::sleep(Duration::from_millis(80)).await;
        let captured = query_on(socket, &["capture-pane", "-p", "-t", "%0"])
            .await
            .unwrap();
        let lines: Vec<_> = captured
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect();
        if ["submit", "deferred-submit"].contains(&scenario) {
            assert_eq!(
                lines.len(),
                2,
                "One Enter must submit exactly one numbered line"
            );
            assert_eq!(lines[0], "READY");
            let (number, delivered) = lines[1].trim().split_once(char::is_whitespace).unwrap();
            assert_eq!(number, "1");
            assert_eq!(delivered.trim_start(), text);
        } else {
            assert_eq!(lines, ["READY"], "{scenario} must not submit any input");
        }
        assert!(
            !query_on(socket, &["list-buffers", "-F", "#{buffer_name}"])
                .await
                .unwrap()
                .contains("sie-dictation-")
        );
    }
}

#[test]
fn client_resolution_rejects_unrelated_ambiguous_and_malformed_targets() {
    let pids = HashSet::from([100, 101]);
    assert_eq!(matching_pane("99\t%1\n100\t%2\n", &pids), Some("%2".into()));
    assert_eq!(matching_pane("100\t%2\n101\t%3\n", &pids), None);
    assert_eq!(matching_pane("100\tsession:0\ninvalid\t%2\n", &pids), None);
    assert_eq!(matching_pane("99\t%1\n", &pids), None);
    assert_eq!(
        matching_pane("100\t%2\n101\t%2\n", &pids),
        Some("%2".into())
    );
}
#[test]
fn agent_titles_and_commands_are_distinct_from_plain_shells() {
    for name in ["codex-sie", "Claude Code", "opencode", "Gemini CLI"] {
        assert!(agent_name(name));
    }
    for name in ["bash", "python3", "gnome-terminal"] {
        assert!(!agent_name(name));
    }
}
