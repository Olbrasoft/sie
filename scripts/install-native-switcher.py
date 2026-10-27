#!/usr/bin/env python3
"""Install the native Shell adapter without restarting the user's session."""
import os
from pathlib import Path
import shutil
import subprocess

from gi.repository import Gio

root = Path(__file__).resolve().parent.parent
uuid = "sie-switcher@olbrasoft.cz"
destination = Path(os.environ.get("XDG_DATA_HOME", str(Path.home() / ".local/share"))) / "gnome-shell/extensions" / uuid
destination.mkdir(parents=True, exist_ok=True)
for name in ["extension.js", "controller.js", "metadata.json"]:
    shutil.copyfile(root / "gnome-extension" / name, destination / name)
shutil.copyfile(root / "LICENSE", destination / "LICENSE")

settings = Gio.Settings.new("org.gnome.shell")
enabled = settings.get_strv("enabled-extensions")
if uuid not in enabled:
    if not settings.set_strv("enabled-extensions", [*enabled, uuid]):
        raise RuntimeError("Could not enable the native SIE switcher")
Gio.Settings.sync()
# GNOME 48 discovers new local extensions at the next session startup. Do not
# restart Shell or disable existing extensions to force discovery.
result = subprocess.run(["gnome-extensions", "enable", uuid], capture_output=True, text=True)
if result.returncode:
    print("Native SIE switcher installed and enabled for the next login. Log out and back in once to load it.")
else:
    print("Native SIE switcher enabled. Updated Shell JavaScript is loaded at the next login.")
