# Repository instructions

- Communicate with the user in Czech. Write code, comments, identifiers, issues
  and commit messages in English.
- Create an issue before implementation. Use one primary label and native
  GraphQL `addSubIssue` relations for parent/child tasks; text links are not a
  substitute. Do not close issues without explicit user approval.
- Keep the desktop application primarily in Rust. Reuse installed GNOME and
  OpenRazer interfaces before introducing additional services or extensions.
- Do not read, record or log ordinary typing. Do not grab the whole keyboard.
- Validate supported USB IDs and matrix dimensions before hardware writes.
- Ignore macro targets that do not exist; never create workspaces implicitly.
- Before submitting changes run `cargo fmt --all -- --check`,
  `cargo clippy --locked --all-targets -- -D warnings` and
  `cargo test --locked --all-targets`. Keep automated tests isolated from real
  devices and the user's desktop. Report physical checks separately.
- Start every PR body with the Codex session marker obtained from
  `~/.codex/get-session-id.sh`. Explain behavior and what needs verification.
