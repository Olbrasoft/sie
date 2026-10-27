"""Question protocol/lifecycle tests without Codex, VA, or the live desktop."""
import asyncio
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("question_observer", Path(__file__).resolve().parents[1] / "scripts/codex-question-notify.py")
observer_module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(observer_module)


class Voice:
    def __init__(self, already_sent=False, fail=False):
        self.already_sent = already_sent
        self.fail = fail
        self.posts = []
        self.remembered = []

    def was_sent(self, key):
        return self.already_sent or key in self.remembered

    def prepare(self, questions):
        return "Mám otázku.", "Úvod"

    def post(self, message):
        if self.fail:
            raise OSError("Voice unavailable")
        self.posts.append(message)

    def remember(self, key, intro):
        self.remembered.append(key)


class Desktop:
    def __init__(self, fail=False):
        self.started = []
        self.canceled = []
        self.fail = fail

    async def show(self, thread_id, cwd, questions):
        self.started.append((thread_id, cwd))
        if self.fail:
            raise FileNotFoundError("No helper")
        try:
            await asyncio.Future()
        finally:
            self.canceled.append(thread_id)


async def until(predicate):
    async with asyncio.timeout(3):
        while not predicate():
            await asyncio.sleep(0.01)


class ObserverTests(unittest.IsolatedAsyncioTestCase):
    def make(self, voice=None, desktop=None):
        observer = observer_module.Observer(None, voice or Voice(), desktop=desktop or Desktop())
        observer.thread_contexts = {"thread-1": "/project", "thread-2": "/other"}
        self.observers.append(observer)
        return observer

    async def asyncSetUp(self):
        self.observers = []

    async def asyncTearDown(self):
        tasks = [task for observer in self.observers for task in observer.tasks]
        for task in tasks:
            task.cancel()
        await asyncio.gather(*tasks, return_exceptions=True)

    def ask(self, observer, thread="thread-1", identifier=1):
        observer.handle({"id": identifier, "method": observer_module.QUESTION_METHOD,
                         "params": {"threadId": thread, "itemId": "question-1", "questions": [{"question": "Funguje to?"}]}})

    async def test_spoken_replay_dedup_does_not_prevent_a_desktop_question_and_resolution_removes_it(self):
        voice = Voice(already_sent=True)
        desktop = Desktop()
        observer = self.make(voice, desktop)
        self.ask(observer)
        await until(lambda: desktop.started)
        self.assertEqual(desktop.started, [("thread-1", "/project")])
        self.assertEqual(voice.posts, [])
        observer.handle({"method": "serverRequest/resolved", "params": {"threadId": "thread-1", "requestId": 1}})
        await until(lambda: desktop.canceled)
        self.assertEqual(desktop.canceled, ["thread-1"])

    async def test_user_reply_cancels_only_its_own_thread(self):
        desktop = Desktop()
        observer = self.make(desktop=desktop)
        self.ask(observer)
        self.ask(observer, "thread-2", 2)
        await until(lambda: len(desktop.started) == 2)
        observer.handle({"method": "item/started", "params": {"threadId": "thread-1", "item": {"type": "userMessage"}}})
        await until(lambda: desktop.canceled)
        self.assertEqual(desktop.canceled, ["thread-1"])
        observer.handle({"method": "thread/closed", "params": {"threadId": "thread-2"}})
        await until(lambda: len(desktop.canceled) == 2)

    async def test_audio_and_desktop_failures_are_independent(self):
        voice = Voice()
        observer = self.make(voice, Desktop(fail=True))
        self.ask(observer)
        await until(lambda: voice.remembered)
        self.assertEqual(len(voice.posts), 1)
        desktop = Desktop()
        observer = self.make(Voice(fail=True), desktop)
        self.ask(observer)
        await until(lambda: desktop.started)
        self.assertEqual(desktop.started[0][0], "thread-1")

    async def test_native_and_async_card_replays_do_not_duplicate_notifications(self):
        voice = Voice()
        desktop = Desktop()
        observer = self.make(voice, desktop)
        self.ask(observer)
        observer.handle({"method": "item/completed", "params": {"threadId": "thread-1", "item": {
            "id": "question-1", "type": "agentMessage", "questions": [{"title": "Funguje to?"}],
        }}})
        await until(lambda: desktop.started and voice.remembered)
        self.assertEqual(len(desktop.started), 1)
        self.assertEqual(len(voice.posts), 1)

    async def test_fast_resolution_prevents_both_deliveries(self):
        voice = Voice()
        desktop = Desktop()
        observer = self.make(voice, desktop)
        self.ask(observer)
        observer.handle({"method": "serverRequest/resolved", "params": {"threadId": "thread-1", "requestId": 1}})
        await asyncio.sleep(0.3)
        self.assertEqual(voice.posts, [])
        self.assertEqual(desktop.started, [])

    async def test_immediate_user_reply_clears_children_that_have_not_started_yet(self):
        observer = self.make()
        self.ask(observer)
        observer.handle({"method": "item/started", "params": {"threadId": "thread-1", "item": {"type": "userMessage"}}})
        await asyncio.sleep(0)
        self.assertEqual(observer.pending, {})
        self.assertEqual(observer.desktop_pending, {})

    async def test_helper_gets_question_on_stdin_and_is_terminated_when_resolved(self):
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            helper = directory / "helper"
            started = directory / "started.json"
            closed = directory / "closed"
            helper.write_text("#!/usr/bin/python3\nimport json, signal, sys, time\nfrom pathlib import Path\n"
                              f"signal.signal(signal.SIGTERM, lambda *_: (Path({str(closed)!r}).touch(), sys.exit(0)))\n"
                              f"Path({str(started)!r}).write_text(json.dumps({{'argv':sys.argv,'payload':json.load(sys.stdin)}}))\n"
                              "while True: time.sleep(1)\n")
            helper.chmod(0o700)
            desktop = observer_module.DesktopNotifications(helper)
            task = asyncio.create_task(desktop.show("thread-1", "/project", [{"question": "Soukromá otázka"}]))
            try:
                await until(started.exists)
                data = json.loads(started.read_text())
                self.assertEqual(data["argv"], [str(helper)])
                self.assertEqual(data["payload"]["text"], "Soukromá otázka")
            finally:
                task.cancel()
                await asyncio.gather(task, return_exceptions=True)
            self.assertTrue(closed.exists())


if __name__ == "__main__":
    unittest.main()
