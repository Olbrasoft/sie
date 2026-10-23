#!/usr/bin/env bash
set -euo pipefail

project_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
data_dir=${XDG_DATA_HOME:-$HOME/.local/share}
config_dir=${XDG_CONFIG_HOME:-$HOME/.config}

if [[ "${SIE_CPU_ONLY:-0}" != 1 && -x /usr/local/cuda/bin/nvcc ]] && command -v nvidia-smi >/dev/null; then
    cuda_arch=$(nvidia-smi --query-gpu=compute_cap --format=csv,noheader | head -n 1 | tr -d '.[:space:]')
    CUDACXX=/usr/local/cuda/bin/nvcc CUDA_PATH=/usr/local/cuda \
        CMAKE_CUDA_ARCHITECTURES="${CMAKE_CUDA_ARCHITECTURES:-$cuda_arch}" \
        cargo build --locked --release --features cuda --manifest-path "$project_root/Cargo.toml"
else
    cargo build --locked --release --manifest-path "$project_root/Cargo.toml"
fi
install -Dm755 "$project_root/target/release/sie" "$HOME/.local/bin/sie"
install -Dm755 "$project_root/target/release/sie-browser-host" "$HOME/.local/bin/sie-browser-host"
python3 "$project_root/scripts/install-browser-bridge.py"
install -Dm644 "$project_root/packaging/sie.service" "$config_dir/systemd/user/sie.service"
install -Dm644 "$project_root/packaging/cz.olbrasoft.SystemInterfaceExtensions.desktop" \
    "$data_dir/applications/cz.olbrasoft.SystemInterfaceExtensions.desktop"
install -Dm644 "$project_root/LICENSE" "$data_dir/sie/licenses/SIE.txt"
install -Dm644 "$project_root/assets/sounds/LICENSE.VirtualAssistant" \
    "$data_dir/sie/licenses/VirtualAssistant.txt"
systemctl --user daemon-reload
printf '%s\n' 'SIE installed. Start with: systemctl --user start sie.service'
