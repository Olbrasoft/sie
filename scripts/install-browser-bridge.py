#!/usr/bin/env python3
"""Install local native-messaging assets; load the extension from Chrome's UI."""
import json
import os
from pathlib import Path
import shutil

root = Path(__file__).resolve().parent.parent
config = Path(os.environ.get('XDG_CONFIG_HOME', str(Path.home()/'.config')))
data = Path(os.environ.get('XDG_DATA_HOME', str(Path.home()/'.local/share')))
folder = data/'sie/browser-extension'
folder.mkdir(parents=True, exist_ok=True)
for name in ['manifest.json', 'background.mjs', 'extension-id.txt']:
    shutil.copyfile(root/'browser-extension'/name, folder/name)
ident = (folder/'extension-id.txt').read_text().strip()
manifest = {
    'name': 'cz.olbrasoft.sie',
    'description': 'Local SIE media tab activation bridge',
    'path': str(Path.home()/'.local/bin/sie-browser-host'),
    'type': 'stdio',
    'allowed_origins': [f'chrome-extension://{ident}/'],
}
for browser in ['google-chrome', 'chromium', 'microsoft-edge']:
    base = config/browser
    if base.exists():
        hosts = base/'NativeMessagingHosts'
        hosts.mkdir(parents=True, exist_ok=True)
        (hosts/'cz.olbrasoft.sie.json').write_text(json.dumps(manifest, indent=2)+'\n')
print(f'Browser bridge assets installed: {folder}')
