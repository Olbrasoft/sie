"""Exercise the actual hook with private SQLite state and stubbed side effects."""
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "scripts/codex-notify.sh"
THREAD = "11111111-2222-4333-8444-555555555555"


class NotifyTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.home = self.root / "home"
        self.home.mkdir()
        self.codex = self.home / ".codex"
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.requests = self.root / "requests.jsonl"
        self.logs = self.root / "logs.jsonl"
        self.environment = dict(os.environ, HOME=str(self.home), CODEX_HOME=str(self.codex),
                                PATH=str(self.bin) + os.pathsep + os.environ["PATH"],
                                TEST_REQUESTS=str(self.requests), TEST_LOGS=str(self.logs),
                                SIE_NOTIFICATION_API_URL="http://127.0.0.1:9/test-only")
        self.environment.pop("VIRTUAL_ASSISTANT_URL", None)
        self.executable("curl", '''#!/usr/bin/python3
import json, os, sys
from pathlib import Path
args=sys.argv[1:]
with Path(os.environ['TEST_REQUESTS']).open('a') as stream:
    stream.write(json.dumps(json.loads(args[args.index('-d')+1]))+'\\n')
''')
        self.executable("logger", '''#!/usr/bin/python3
import json, os, sys
from pathlib import Path
with Path(os.environ['TEST_LOGS']).open('a') as stream:
    stream.write(json.dumps(sys.argv[1:])+'\\n')
''')

    def executable(self, name, content):
        path = self.bin / name
        path.write_text(content)
        path.chmod(0o700)

    def registry(self, persisted=False):
        self.codex.mkdir(parents=True, exist_ok=True)
        with sqlite3.connect(self.codex / "state_5.sqlite") as connection:
            connection.execute("CREATE TABLE threads (id TEXT PRIMARY KEY)")
            if persisted:
                connection.execute("INSERT INTO threads VALUES (?)", (THREAD,))

    def run_hook(self, text="Oprava funguje.", event="agent-turn-complete", thread=THREAD):
        payload = {"type": event, "thread-id": thread, "cwd": "/test/project",
                   "last-assistant-message": text, "input-messages": ["PRIVATE INPUT MUST NOT BE LOGGED"]}
        result = subprocess.run(["bash", str(SCRIPT), json.dumps(payload)], env=self.environment,
                                cwd=self.root, capture_output=True, text=True, timeout=4)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "")
        self.assertEqual(result.stderr, "")
        if self.logs.exists():
            self.assertNotIn("PRIVATE INPUT", self.logs.read_text())
            self.assertNotIn(text, self.logs.read_text())
        return [json.loads(line) for line in self.requests.read_text().splitlines()] if self.requests.exists() else []

    def test_persisted_completion_speaks_only_the_clean_final_paragraph_and_indexes_the_session(self):
        self.registry(persisted=True)
        requests = self.run_hook("Technical report.\n\n> **Opravil jsem** [diktování](https://example.test). `Pause` funguje.")
        self.assertEqual([r["text"] for r in requests], ["Opravil jsem diktování. Pause funguje."])
        self.assertEqual(requests[0]["source"], "codex")
        indexes = list((self.codex / "sessions/pids").glob("*.json"))
        self.assertEqual(len(indexes), 1)
        self.assertEqual(json.loads(indexes[0].read_text())["sessionId"], THREAD)

    def test_nonpersisted_completion_neither_speaks_nor_replaces_existing_pid_routing(self):
        self.registry()
        folder = self.codex / "sessions/pids"
        folder.mkdir(parents=True)
        existing = folder / f"{os.getpid()}.json"
        original = json.dumps({"pid": os.getpid(), "sessionId": "original-thread", "cwd": "/original"})
        existing.write_text(original)
        self.assertEqual(self.run_hook("Internal generated summary."), [])
        self.assertEqual(existing.read_text(), original)
        self.assertIn("Skipped completion", self.logs.read_text())

    def test_missing_registry_preserves_legacy_behavior_without_creating_a_database(self):
        self.assertEqual(len(self.run_hook()), 1)
        self.assertFalse((self.codex / "state_5.sqlite").exists())

    def test_incompatible_registry_preserves_legacy_behavior(self):
        self.codex.mkdir()
        with sqlite3.connect(self.codex / "state_5.sqlite") as connection:
            connection.execute("CREATE TABLE older_schema (id TEXT)")
        self.assertEqual(len(self.run_hook()), 1)

    def test_unreadable_registry_and_lookup_deadline_do_not_hold_up_normal_notifications(self):
        self.registry(persisted=True)
        connection = sqlite3.connect(self.codex / "state_5.sqlite")
        try:
            connection.execute("BEGIN EXCLUSIVE")
            self.assertEqual(len(self.run_hook()), 1)
        finally:
            connection.rollback()
            connection.close()
        self.executable("python3", "#!/bin/sh\nsleep 5\n")
        self.assertEqual(len(self.run_hook()), 2)

    def test_custom_codex_home_is_used_for_both_validation_and_pid_index(self):
        self.codex = self.root / "custom codex state"
        self.environment["CODEX_HOME"] = str(self.codex)
        self.registry(persisted=True)
        self.assertEqual(len(self.run_hook()), 1)
        self.assertTrue((self.codex / "sessions/pids").exists())
        self.assertFalse((self.home / ".codex").exists())

    def test_other_events_and_empty_messages_do_not_speak(self):
        self.registry(persisted=True)
        self.assertEqual(self.run_hook(event="agent-turn-start"), [])
        self.assertEqual(self.run_hook(text=" \n\n "), [])

    def test_unicode_summary_still_obeys_the_existing_output_bound(self):
        self.registry(persisted=True)
        text = self.run_hook("ž" * 400)[0]["text"]
        self.assertEqual(text, "ž" * 347 + "...")

    def test_thread_lookup_uses_a_bound_parameter(self):
        self.registry(persisted=True)
        self.assertEqual(self.run_hook(thread="' OR 1=1 --"), [])


if __name__ == "__main__":
    unittest.main()
