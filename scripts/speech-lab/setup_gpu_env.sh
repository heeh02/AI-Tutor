#!/usr/bin/env bash
# Isolated WSL inference runtime. Does not alter system Python, drivers or PATH.
set -euo pipefail
task_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
task_uv="$task_root/runtime/tools/uv-linux/uv-x86_64-unknown-linux-gnu/uv"
task_env="$task_root/runtime/speech-gpu/omni"
export UV_CACHE_DIR="$task_root/runtime/speech-gpu/uv-cache"
export UV_PYTHON_INSTALL_DIR="$task_root/runtime/speech-gpu/python"
export UV_HTTP_TIMEOUT=120
export UV_NO_PROGRESS=1
export HF_HOME="$task_root/models/speech/hf-cache"
export XDG_CACHE_HOME="$task_root/runtime/speech-gpu/cache"
mkdir -p "$task_root/runtime/goal-baselines/20261104"
"$task_uv" --no-config python install 3.12.14 --no-bin
"$task_uv" --no-config venv --python 3.12.14 "$task_env"
"$task_uv" --no-config pip install --python "$task_env/bin/python" \
    --torch-backend=cu130 'vllm==0.32.0' 'vllm-omni==0.32.0'
"$task_uv" --no-config pip freeze --python "$task_env/bin/python" \
    > "$task_root/runtime/goal-baselines/20261104/gpu-environment-freeze.txt"
"$task_env/bin/python" - <<'PY'
import json, platform, torch
from importlib.metadata import version
report = dict(python=platform.python_version(), torch=torch.__version__,
              cuda_runtime=torch.version.cuda, cuda_available=torch.cuda.is_available(),
              vllm=version('vllm'), vllm_omni=version('vllm-omni'))
if torch.cuda.is_available():
    report.update(device=torch.cuda.get_device_name(0), capability=torch.cuda.get_device_capability(0),
                  total_vram=torch.cuda.get_device_properties(0).total_memory)
    tensor = torch.arange(16, device='cuda', dtype=torch.float32)
    report['cuda_kernel_check'] = float((tensor * tensor).sum().cpu()) == 1240.0
print(json.dumps(report))
if not report['cuda_available'] or not report.get('cuda_kernel_check'):
    raise SystemExit('GPU inference runtime is not ready')
PY
