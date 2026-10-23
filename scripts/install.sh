#!/usr/bin/env bash
set -euo pipefail

project_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
data_dir=${XDG_DATA_HOME:-$HOME/.local/share}
config_dir=${XDG_CONFIG_HOME:-$HOME/.config}

cargo build --locked --release --manifest-path "$project_root/Cargo.toml"
install -Dm755 "$project_root/target/release/sie" "$HOME/.local/bin/sie"
install -Dm644 "$project_root/packaging/sie.service" "$config_dir/systemd/user/sie.service"
install -Dm644 "$project_root/packaging/cz.olbrasoft.SystemInterfaceExtensions.desktop" \
    "$data_dir/applications/cz.olbrasoft.SystemInterfaceExtensions.desktop"
systemctl --user daemon-reload
printf '%s\n' 'SIE installed. Start with: systemctl --user start sie.service'
