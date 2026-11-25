#!/usr/bin/env bash
# Reuse the exact downloaded wheels on WSL's native filesystem. The previous
# Windows-mounted environment is preserved for comparison and rollback.
set -euo pipefail
task_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
task_uv="$task_root/runtime/tools/uv-linux/uv-x86_64-unknown-linux-gnu/uv"
task_local_root="${XDG_DATA_HOME:-$HOME/.local/share}/ai-tutor-gpu"
task_env="$task_local_root/omni-0.32.0"
export UV_CACHE_DIR="$task_root/runtime/speech-gpu/uv-cache"
export UV_PYTHON_INSTALL_DIR="$task_local_root/python"
export UV_NO_PROGRESS=1
mkdir -p "$task_local_root"
python3 - "$task_local_root" <<'PY'
import shutil, sys
free = shutil.disk_usage(sys.argv[1]).free
if free < 20 * 1024**3:
    raise SystemExit('Need 20GiB free for the 8.64GiB environment and a filesystem reserve')
print(f'native_filesystem_free_gib={free/1024**3:.2f}', flush=True)
PY
"$task_uv" --no-config python install 3.12.14 --no-bin
"$task_uv" --no-config venv --python 3.12.14 "$task_env"
"$task_uv" --no-config pip install --link-mode=copy --python "$task_env/bin/python" \
    --torch-backend=cu130 -r "$task_root/scripts/speech-lab/requirements-omni-linux.txt"
"$task_env/bin/python" "$task_root/scripts/speech-lab/verify_gpu_env.py" \
    "$task_root/runtime/speech-gpu/omni-env-path.txt"
