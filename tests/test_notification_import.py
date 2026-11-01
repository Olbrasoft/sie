"""Synthetic import settings; never read the real vault or desktop keyring."""
import importlib.util
import datetime
import json
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("notification_import", Path(__file__).resolve().parents[1] / "scripts/import-notifications.py")
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class ImportTests(unittest.TestCase):
    def test_voice_chain_and_credential_references_exclude_secret_values(self):
        names = ["GoogleCloudMultiKey", "AzureTTS", "EdgeTTS-WebSocket", "VoiceRSS", "GoogleTTS", "Piper"]
        source = {"TTS": {"Orchestration": {"Providers": [{"Name": n, "Priority": i} for i, n in enumerate(names)]}},
                  "TtsProfiles": {"Profiles": {"codex": {"Provider": names[0], "Voice": "cs-CZ-Chirp3-HD-Enceladus"}},
                                  "DefaultProfile": {"Provider": "Piper", "Voice": "cs_CZ-jirka-medium"}},
                  "NotificationAudio": {"AudioSink": "test.monitor"}}
        for name in names:
            source["TTS"]["EdgeTTS" if name == "EdgeTTS-WebSocket" else name] = {"Voice": "test-voice", "ApiKey": "FAKE-SECRET-MUST-NOT-BE-COPIED"}
        source["TTS"]["GoogleCloudMultiKey"]["ApiKeySecrets"] = [{"Name": "one", "SecretKey": "vault-one"}, {"Name": "two", "SecretKey": "vault-two"}]
        source["TTS"]["Piper"]["ModelPath"] = "/test/model.onnx"
        result, selection = module.prepare(source)
        self.assertEqual([p["name"] for p in result["providers"]], names)
        self.assertEqual(result["profiles"]["codex"]["voice"], "cs-CZ-Chirp3-HD-Enceladus")
        self.assertEqual([c["reference"] for c in result["providers"][0]["credentials"]], ["google/one", "google/two"])
        self.assertNotIn("FAKE-SECRET", json.dumps(result) + json.dumps(selection))
        self.assertEqual(selection[0]["vaultKey"], "vault-one")

    def test_quota_exhaustion_is_not_migrated_as_a_permanent_invalid_key(self):
        calls = []
        original = module.psql
        def fake(database, sql):
            calls.append((database, sql))
            if database == "old":
                now = datetime.datetime.now(datetime.timezone.utc)
                return json.dumps({"key_name": "one", "counter_year": now.year, "counter_month": now.month,
                                   "monthly_character_count": 42, "state": 2, "cooldown_until_utc": None})
            return ""
        module.psql = fake
        try:
            module.import_usage("old", "test_sie_private", {"providers": [{"name": "GoogleCloudMultiKey", "credentials": [{"name": "one", "reference": "google/one"}]}]})
        finally:
            module.psql = original
        self.assertIn("42,NULL,FALSE", calls[-1][1])
        self.assertIn("tts_key_usage", calls[1][1])
        self.assertNotIn("dictation_history", " ".join(sql for _, sql in calls))


if __name__ == "__main__":
    unittest.main()
