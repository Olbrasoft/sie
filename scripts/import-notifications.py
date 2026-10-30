#!/usr/bin/env python3
"""One-time notification settings/keyring/quota import; never imports old messages."""
import argparse
import datetime
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import tomllib


def profile(value):
    return {
        "provider": "EdgeTTS-WebSocket" if value["Provider"] == "EdgeTTS" else value["Provider"],
        "voice": value["Voice"], "rate": value.get("Rate", "+0%"),
        "volume": value.get("Volume", "+0%"), "pitch": value.get("Pitch", "+0Hz"),
    }


def prepare(source):
    tts = source["TTS"]
    selections = []
    providers = []
    order = sorted(tts["Orchestration"]["Providers"], key=lambda p: p["Priority"])
    for entry in order:
        if not entry.get("Enabled", True):
            continue
        name = entry["Name"]
        section = tts["EdgeTTS" if name == "EdgeTTS-WebSocket" else name]
        provider = {"name": name, "voice": section.get("Voice", "cs_CZ-jirka-medium"),
                    "timeout_seconds": section.get("TimeoutSeconds", 30)}
        if name == "GoogleCloudMultiKey":
            provider["credentials"] = []
            for key in section["ApiKeySecrets"]:
                ref = "google/" + key["Name"]
                provider["credentials"].append({"name": key["Name"], "reference": ref,
                                               "monthly_limit": section.get("MonthlyCharacterLimit", 1_000_000)})
                selections.append({"vaultKey": key["SecretKey"], "reference": ref, "required": True})
        elif name in {"AzureTTS", "VoiceRSS"}:
            ref = "azure" if name == "AzureTTS" else "voicerss"
            provider["credentials"] = [{"name": name, "reference": ref}]
            selections.append({"vaultKey": "TTS:" + name + (":SubscriptionKey" if name == "AzureTTS" else ":ApiKey"),
                               "reference": ref, "required": False})
            if name == "AzureTTS":
                provider["region"] = section.get("Region", "westeurope")
        elif name in {"EdgeTTS-WebSocket", "Piper", "GoogleTTS"}:
            command = {"EdgeTTS-WebSocket": "edge-tts", "Piper": "piper", "GoogleTTS": "gtts-cli"}[name]
            provider["executable"] = shutil.which(command) or command
            if name == "Piper":
                provider["model_path"] = str(Path(section["ModelPath"]).expanduser())
                provider["length_scale"] = section.get("Profiles", {}).get("default", {}).get("LengthScale", 0.5)
                provider["timeout_seconds"] = section.get("TimeoutSeconds", 60)
        else:
            raise ValueError("Unsupported provider in source settings")
        providers.append(provider)
    profiles = {name: profile(value) for name, value in source["TtsProfiles"]["Profiles"].items()}
    if "claude-code" in profiles:
        profiles.setdefault("claudecode", profiles["claude-code"])
    result = {"providers": providers, "profiles": profiles,
              "default_profile": profile(source["TtsProfiles"]["DefaultProfile"]),
              "sound_sink": source["NotificationAudio"]["AudioSink"]}
    return result, selections


def psql(database, sql):
    # Same optional one-time dependency as the existing dictation importer.
    # Connection strings never enter process arguments, environment or logs.
    import getpass
    import psycopg2
    if "=" in database or database.startswith(("postgres://", "postgresql://")):
        connection = psycopg2.connect(database)
    else:
        connection = psycopg2.connect(dbname=database, host="/var/run/postgresql", user=getpass.getuser())
    try:
        with connection, connection.cursor() as cursor:
            cursor.execute(sql)
            if not cursor.description:
                return ""
            return "\n".join(json.dumps(row[0]) if isinstance(row[0], dict) else str(row[0]) for row in cursor.fetchall())
    finally:
        connection.close()


def import_usage(source_db, target_db, settings):
    root = Path(__file__).resolve().parent.parent
    psql(target_db, (root / "migrations/002_notifications.sql").read_text())
    rows = psql(source_db, "SELECT row_to_json(t) FROM (SELECT key_name,counter_year,counter_month,monthly_character_count,state,cooldown_until_utc FROM tts_key_usage) t;")
    references = {c["name"]: c["reference"] for p in settings["providers"]
                  if p["name"] == "GoogleCloudMultiKey" for c in p.get("credentials", [])}
    count = 0
    def quoted(value):
        return "'" + str(value).replace("'", "''") + "'"
    for line in rows.splitlines():
        row = json.loads(line)
        now = datetime.datetime.now(datetime.timezone.utc)
        if (row["counter_year"], row["counter_month"]) != (now.year, now.month):
            continue
        ref = references.get(row["key_name"])
        if not ref:
            continue
        month = datetime.date(row["counter_year"], row["counter_month"], 1).isoformat()
        chars = max(0, int(row["monthly_character_count"]))
        cooldown = quoted(row["cooldown_until_utc"]) if row["cooldown_until_utc"] else "NULL"
        # Invalid is enum value 3 in the original provider. Quota/rate-limit
        # states use their persisted expiry, not permanent credential disabling.
        disabled = "TRUE" if row["state"] == 3 else "FALSE"
        psql(target_db, f"""INSERT INTO notification_key_usage(reference,counter_month,characters,cooldown_until,disabled)
VALUES ({quoted(ref)},{quoted(month)},{chars},{cooldown},{disabled})
ON CONFLICT(reference) DO UPDATE SET
characters=CASE WHEN notification_key_usage.counter_month=EXCLUDED.counter_month
THEN GREATEST(notification_key_usage.characters,EXCLUDED.characters) ELSE EXCLUDED.characters END,
counter_month=EXCLUDED.counter_month,
cooldown_until=GREATEST(notification_key_usage.cooldown_until,EXCLUDED.cooldown_until),
disabled=notification_key_usage.disabled OR EXCLUDED.disabled;""")
        count += 1
    return count


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-settings", type=Path, default=Path("/opt/olbrasoft/virtual-assistant/config/appsettings.json"))
    parser.add_argument("--sie-config", type=Path, default=Path.home() / ".config/sie/config.toml")
    parser.add_argument("--source-db", default="virtual_assistant")
    parser.add_argument("--apply", action="store_true")
    args = parser.parse_args()
    source = json.loads(args.source_settings.read_text())
    settings, selection = prepare(source)
    print(f"Prepared {len(settings['profiles'])} source voices and {len(settings['providers'])} TTS providers; no old notification bodies will be copied.")
    if not args.apply:
        return
    config = tomllib.loads(args.sie_config.read_text())
    target_db = config["dictation"]["database_url"]
    if "virtual_assistant" in target_db:
        raise ValueError("Refusing to use the VirtualAssistant database as the target")
    root = Path(__file__).resolve().parent.parent
    with tempfile.TemporaryDirectory(prefix="sie-credential-selection-") as directory:
        selected = Path(directory) / "selection.json"
        selected.write_text(json.dumps(selection))
        selected.chmod(0o600)
        store = source.get("SecureStore", {})
        vault = Path(store.get("SecretsPath", "~/.config/virtual-assistant/secrets/secrets.json")).expanduser()
        key = Path(store.get("KeyPath", "~/.config/virtual-assistant/keys/secrets.key")).expanduser()
        subprocess.run(["dotnet", "run", "--project", str(root / "tools/import-va-secrets/ImportSecrets.csproj"),
                        "--", str(vault), str(key), str(selected)], check=True)
    destination = args.sie_config.with_name("notifications.json")
    destination.parent.mkdir(parents=True, exist_ok=True)
    if destination.exists():
        backup = destination.with_suffix(".backup-" + datetime.datetime.now().strftime("%Y%m%d%H%M%S"))
        shutil.copy2(destination, backup)
        backup.chmod(0o600)
    with tempfile.NamedTemporaryFile(mode="w", dir=destination.parent, delete=False) as temporary:
        os.chmod(temporary.name, 0o600)
        json.dump(settings, temporary, ensure_ascii=False, indent=2)
    os.replace(temporary.name, destination)
    count = import_usage(args.source_db, target_db, settings)
    print(f"Installed notification settings and preserved {count} key-usage counters. Enable [notifications] in SIE only when ready to switch the API port.")


if __name__ == "__main__":
    main()
