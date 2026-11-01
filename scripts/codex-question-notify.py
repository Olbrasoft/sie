#!/usr/bin/env python3
"""Preserve spoken questions and show clickable questions through SIE."""

import asyncio
import contextlib
import hashlib
import json
import logging
import os
from pathlib import Path
import random
import re
import sqlite3
import time
import unicodedata
import urllib.request


LOG = logging.getLogger("codex-question-notify")
QUESTION_METHOD = "item/tool/requestUserInput"
MAX_SPOKEN_LENGTH = 250
INTRODUCTIONS = {
    "verification": (
        "Prosím o ověření výsledku.",
        "Potřebuji vědět, jestli už vše funguje.",
        "V Codexu čekám na výsledek tvé kontroly.",
    ),
    "choice": (
        "Potřebuji od tebe vybrat další postup.",
        "V Codexu na tebe čeká otázka s možnostmi.",
        "Čekám na tvou volbu, jak pokračovat.",
    ),
    "question": (
        "Mám na tebe otázku.",
        "Potřebuji od tebe upřesnění.",
        "V Codexu čekám na tvoji odpověď.",
    ),
}
# Suppress high-volume notifications at the server, rather than logging or
# processing terminal output and model deltas. Never suppress server requests.
MUTED_NOTIFICATIONS = [
    "item/commandExecution/outputDelta",
    "item/fileChange/outputDelta",
    "item/agentMessage/delta",
    "item/reasoning/textDelta",
    "item/reasoning/summaryTextDelta",
    "item/plan/delta",
    "thread/tokenUsage/updated",
]


def spoken_text(value):
    """Remove formatting and technical paths that sound confusing when spoken."""
    if not isinstance(value, str):
        return ""
    value = re.sub(r"```.*?```", " ", value, flags=re.S)
    value = re.sub(r"\[([^\]]+)\]\([^)]+\)", r"\1", value)
    value = re.sub(r"https?://\S+|(?<!\w)(?:~/|/)[\w./-]+", " ", value)
    value = re.sub(r"[`*#_>]", "", value)
    return " ".join(value.split())


def make_message(questions, last_intro=""):
    texts = [spoken_text(q.get("question") or q.get("title")) for q in questions]
    question = next((text for text in texts if text), "Otevři prosím čekající otázku v Codexu.")
    plain = unicodedata.normalize("NFKD", question).encode("ascii", "ignore").decode().lower()
    if re.search(r"fung|over|kontrol|spravn|vysled|ozval|slys|verify|working|works|correct|test", plain):
        category = "verification"
    elif any(q.get("options") for q in questions):
        category = "choice"
    else:
        category = "question"
    choices = [intro for intro in INTRODUCTIONS[category] if intro != last_intro]
    intro = random.choice(choices)
    suffix = " V Codexu čekají i další otázky." if len(questions) > 1 else ""
    budget = MAX_SPOKEN_LENGTH - len(intro) - len(suffix) - 1
    if len(question) > budget:
        question = question[: budget - 1].rsplit(" ", 1)[0].rstrip(".,:;!?") + "…"
    return f"{intro} {question}{suffix}", intro


def request_key(params):
    identity = params.get("itemId") or hashlib.sha256(
        json.dumps(params.get("questions", []), sort_keys=True, ensure_ascii=False).encode()
    ).hexdigest()
    return f"{params['threadId']}:{identity}"


class Delivery:
    def __init__(self, state_path, api_url):
        state_path = Path(state_path)
        state_path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        self.db = sqlite3.connect(state_path)
        self.db.execute("CREATE TABLE IF NOT EXISTS sent (key TEXT PRIMARY KEY, sent_at REAL)")
        self.db.execute("CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT)")
        self.db.execute("DELETE FROM sent WHERE sent_at < ?", (time.time() - 30 * 86400,))
        self.db.commit()
        self.api_url = api_url

    def was_sent(self, key):
        return self.db.execute("SELECT 1 FROM sent WHERE key = ?", (key,)).fetchone() is not None

    def prepare(self, questions):
        row = self.db.execute("SELECT value FROM settings WHERE key = 'last_intro'").fetchone()
        return make_message(questions, row[0] if row else "")

    def post(self, message):
        body = json.dumps({
            "text": message, "source": "codex", "providerName": "openai", "modelName": "codex",
        }, ensure_ascii=False).encode()
        request = urllib.request.Request(
            self.api_url, data=body, headers={"Content-Type": "application/json"}, method="POST",
        )
        with urllib.request.urlopen(request, timeout=5) as response:
            result = json.load(response)
            if not result.get("success"):
                raise ValueError("The notification API did not acknowledge the notification")

    def remember(self, key, intro):
        self.db.execute("INSERT OR REPLACE INTO sent VALUES (?, ?)", (key, time.time()))
        self.db.execute("INSERT OR REPLACE INTO settings VALUES ('last_intro', ?)", (intro,))
        self.db.commit()


class DesktopNotifications:
    """One owned Rust helper per pending question; no question text in argv."""

    def __init__(self, executable):
        self.executable = str(executable)

    async def show(self, thread_id, cwd, questions):
        if not isinstance(cwd, str) or not Path(cwd).is_absolute():
            LOG.info("Desktop question skipped: thread directory is unavailable")
            return
        texts = [q.get("question") or q.get("title") for q in questions]
        text = "\n".join(t for t in texts if isinstance(t, str))
        text = text.encode("utf-8")[:12000].decode("utf-8", errors="ignore")
        payload = json.dumps({
            "agent": "codex", "session_id": thread_id, "cwd": cwd, "text": text,
        }, ensure_ascii=False).encode()
        process = await asyncio.create_subprocess_exec(
            self.executable, stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.DEVNULL, stderr=asyncio.subprocess.DEVNULL,
        )
        try:
            process.stdin.write(payload)
            await process.stdin.drain()
            process.stdin.close()
            await process.stdin.wait_closed()
            if await process.wait() != 0:
                LOG.warning("Desktop question helper could not display or activate the origin")
        finally:
            if process.returncode is None:
                with contextlib.suppress(ProcessLookupError):
                    process.terminate()
                try:
                    await asyncio.wait_for(process.wait(), timeout=3)
                except TimeoutError:
                    with contextlib.suppress(ProcessLookupError):
                        process.kill()
                    await process.wait()


class Observer:
    """Join loaded threads and observe requests without ever answering them."""

    def __init__(self, connection, delivery, poll_seconds=2, desktop=None):
        self.connection = connection
        self.delivery = delivery
        self.poll_seconds = poll_seconds
        self.sequence = 0
        self.responses = {}
        self.subscribed = set()
        self.retry_after = {}
        self.pending = {}
        self.request_keys = {}
        self.tasks = set()
        self.desktop = desktop
        self.desktop_pending = {}
        self.desktop_seen = set()
        self.thread_contexts = {}

    def cancel_thread(self, thread_id):
        for pending in [self.pending, self.desktop_pending]:
            for key, task in list(pending.items()):
                if key.startswith(thread_id + ":"):
                    pending.pop(key, None)
                    task.cancel()

    async def rpc(self, method, params):
        self.sequence += 1
        identifier = f"question-notifier-{self.sequence}"
        future = asyncio.get_running_loop().create_future()
        self.responses[identifier] = future
        try:
            await self.connection.send(json.dumps({"id": identifier, "method": method, "params": params}))
            return await asyncio.wait_for(future, timeout=15)
        finally:
            self.responses.pop(identifier, None)

    def handle(self, message):
        method = message.get("method")
        if method is None:
            future = self.responses.get(message.get("id"))
            if future is not None and not future.done():
                if "error" in message:
                    future.set_exception(RuntimeError(str(message["error"])))
                else:
                    future.set_result(message["result"])
            return
        params = message.get("params", {})
        if method in {"item/started", "item/completed"}:
            item = params.get("item", {})
            if item.get("type") == "userMessage":
                self.cancel_thread(params["threadId"])
            if item.get("type") == "agentMessage" and item.get("questions"):
                # Post-turn question cards use agentMessage.questions, rather
                # than the blocking tool/requestUserInput server request.
                self.enqueue({
                    "threadId": params["threadId"], "itemId": item["id"],
                    "questions": item["questions"],
                })
            return
        if method == QUESTION_METHOD:
            if not params.get("threadId") or not isinstance(params.get("questions"), list):
                LOG.warning("Ignored a malformed question request")
                return
            key = request_key(params)
            self.request_keys[(params["threadId"], message["id"])] = key
            self.enqueue(params)
        elif method == "serverRequest/resolved":
            key = self.request_keys.pop((params.get("threadId"), params.get("requestId")), None)
            for pending in [self.pending, self.desktop_pending]:
                task = pending.pop(key, None)
                if task:
                    task.cancel()
        elif method in {"thread/closed", "thread/archived"}:
            thread_id = params.get("threadId")
            self.subscribed.discard(thread_id)
            if thread_id:
                self.cancel_thread(thread_id)
                self.thread_contexts.pop(thread_id, None)
        # Deliberately send no JSON-RPC response to any server request. The user's
        # existing Codex UI remains responsible for answers, approvals and tools.

    def enqueue(self, params):
        questions = [q for q in params.get("questions", []) if isinstance(q, dict)]
        if not questions:
            return
        key = request_key(params)
        if key not in self.pending and not self.delivery.was_sent(key):
            task = asyncio.create_task(self.deliver(key, questions))
            self.pending[key] = task
            self.tasks.add(task)
            task.add_done_callback(self.tasks.discard)
        if self.desktop and key not in self.desktop_seen:
            self.desktop_seen.add(key)
            task = asyncio.create_task(self.deliver_desktop(key, params["threadId"], questions))
            self.desktop_pending[key] = task
            self.tasks.add(task)
            task.add_done_callback(self.tasks.discard)

    async def deliver_desktop(self, key, thread_id, questions):
        try:
            await asyncio.sleep(0.25)
            await self.desktop.show(thread_id, self.thread_contexts.get(thread_id), questions)
        except (OSError, ValueError, RuntimeError) as error:
            LOG.warning("Desktop question unavailable (%s); spoken delivery remains independent", type(error).__name__)
        finally:
            self.desktop_pending.pop(key, None)

    async def deliver(self, key, questions):
        try:
            # Give a just-answered or replayed-and-resolved request time to clear.
            await asyncio.sleep(0.25)
            message, intro = self.delivery.prepare(questions)
            delay = 5
            while True:
                try:
                    await asyncio.to_thread(self.delivery.post, message)
                    self.delivery.remember(key, intro)
                    LOG.info("Question notification accepted by SIE")
                    return
                except (OSError, ValueError) as error:
                    LOG.warning("Notification delivery failed (%s); retrying in %s seconds", type(error).__name__, delay)
                    await asyncio.sleep(delay)
                    delay = min(delay * 2, 60)
        finally:
            self.pending.pop(key, None)

    async def receive(self):
        try:
            async for raw in self.connection:
                self.handle(json.loads(raw))
        finally:
            for future in self.responses.values():
                if not future.done():
                    future.set_exception(ConnectionError("Codex disconnected"))

    async def watch_threads(self):
        await self.rpc("initialize", {
            "clientInfo": {"name": "codex_question_notify", "version": "1.0.0"},
            "capabilities": {"experimentalApi": True, "optOutNotificationMethods": MUTED_NOTIFICATIONS},
        })
        await self.connection.send(json.dumps({"method": "initialized", "params": {}}))
        LOG.info("Connected to the local Codex daemon")
        while True:
            cursor = None
            loaded = set()
            while True:
                result = await self.rpc("thread/loaded/list", {"cursor": cursor, "limit": 100})
                loaded.update(result["data"])
                cursor = result.get("nextCursor")
                if not cursor:
                    break
            self.subscribed.intersection_update(loaded)
            for thread_id in sorted(loaded - self.subscribed):
                if self.retry_after.get(thread_id, 0) > time.monotonic():
                    continue
                try:
                    # Only rejoin threads the daemon already has in memory. Do
                    # not start turns, restore stored sessions or change settings.
                    result = await self.rpc("thread/resume", {"threadId": thread_id, "excludeTurns": True})
                    self.thread_contexts[thread_id] = result.get("thread", {}).get("cwd")
                    self.subscribed.add(thread_id)
                    LOG.info("Observing questions in thread %s", thread_id)
                    await self.recover_questions(thread_id)
                except RuntimeError as error:
                    if "no rollout found" in str(error):
                        # Empty or stale loaded sessions cannot be joined yet.
                        # Retry promptly so their first real question is covered.
                        self.retry_after[thread_id] = time.monotonic() + self.poll_seconds
                    else:
                        self.retry_after[thread_id] = time.monotonic() + 60
                        LOG.warning("Could not observe thread %s (%s); retrying in 60 seconds", thread_id, error)
            await asyncio.sleep(self.poll_seconds)

    async def recover_questions(self, thread_id):
        # Rejoining replays native server requests, but not async question cards.
        # Read only the latest turn and ignore cards preceding a user reply.
        result = await self.rpc("thread/turns/list", {
            "threadId": thread_id, "limit": 1, "sortDirection": "desc", "itemsView": "summary",
        })
        for turn in result.get("data", []):
            pending = []
            for item in turn.get("items", []):
                if item.get("type") == "userMessage":
                    pending.clear()
                elif item.get("type") == "agentMessage" and item.get("questions"):
                    pending.append(item)
            for item in pending:
                self.enqueue({"threadId": thread_id, "itemId": item["id"], "questions": item["questions"]})

    async def run(self):
        receive = asyncio.create_task(self.receive())
        watch = asyncio.create_task(self.watch_threads())
        try:
            done, _ = await asyncio.wait([receive, watch], return_when=asyncio.FIRST_COMPLETED)
            for task in done:
                task.result()
        finally:
            cleanup = [receive, watch, *self.tasks]
            for task in cleanup:
                task.cancel()
            await asyncio.gather(*cleanup, return_exceptions=True)


async def main():
    import websockets

    codex_dir = Path(os.environ.get("CODEX_HOME", str(Path.home() / ".codex")))
    socket_path = os.environ.get("CODEX_QUESTION_SOCKET", str(codex_dir / "app-server-control/app-server-control.sock"))
    state_dir = Path(os.environ.get("XDG_STATE_HOME", str(Path.home() / ".local/state")))
    delivery = Delivery(
        state_dir / "codex-question-notify/state.sqlite",
        os.environ.get("SIE_NOTIFICATION_API_URL") or os.environ.get("VIRTUAL_ASSISTANT_URL", "http://localhost:5055").rstrip("/") + "/api/notifications",
    )
    desktop = DesktopNotifications(os.environ.get(
        "SIE_AGENT_NOTIFY_BIN", str(Path.home() / ".local/bin/sie-agent-notify"),
    ))
    try:
        while True:
            try:
                async with websockets.unix_connect(socket_path, max_size=16 * 1024 * 1024) as connection:
                    await Observer(connection, delivery, desktop=desktop).run()
            except (OSError, RuntimeError, TimeoutError, websockets.WebSocketException) as error:
                LOG.warning("Codex connection unavailable (%s); reconnecting in 5 seconds", type(error).__name__)
            await asyncio.sleep(5)
    finally:
        delivery.db.close()


if __name__ == "__main__":
    logging.basicConfig(level=logging.INFO, format="%(levelname)s %(message)s")
    with contextlib.suppress(KeyboardInterrupt):
        asyncio.run(main())
