#!/usr/bin/env bash
# Explicit lab service; stop the owned Qwen instance before using this GPU.
set -euo pipefail
task_bundle="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
task_root="$(cd "${AI_TUTOR_PROJECT_ROOT:-$task_bundle/../..}" && pwd)"
IFS= read -r task_env < "$task_root/runtime/speech-gpu/omni-env-path.txt"
export PATH="$task_env/bin:$PATH"
export PYTHONUNBUFFERED=1
source "$task_bundle/tts_gpu_lease.sh"
tutor_tts_gpu_lease "$task_env/bin/python"
source "$task_bundle/cuda_link_env.sh"
export HF_HOME="$task_root/runtime/speech-cosy/hf-cache"
export XDG_CACHE_HOME="$task_root/runtime/speech-cosy/cache"
export HF_HUB_OFFLINE=1
export VLLM_NO_USAGE_STATS=1
export DO_NOT_TRACK=1
export TOKENIZERS_PARALLELISM=false
export COSYVOICE3_TRT=0
export COSYVOICE3_BATCH_FLOW=0
export COSYVOICE3_PACKED_STREAMING=0
export COSYVOICE3_FULL_RESPONSE_OPTIMIZATIONS=0
export COSYVOICE3_REFERENCE_WORKERS=1
if [[ -n "${AI_TUTOR_MANAGED_TTS_TOKEN:-}" ]]; then
  export VLLM_API_KEY="$AI_TUTOR_MANAGED_TTS_TOKEN"
  unset AI_TUTOR_MANAGED_TTS_TOKEN
else
  task_token_path="$task_root/runtime/speech-cosy/api-key"
  if [[ ! -f "$task_token_path" ]]; then
    "$task_env/bin/python" -c 'import secrets,sys; from pathlib import Path; Path(sys.argv[1]).write_text(secrets.token_urlsafe(32))' "$task_token_path"
  fi
  export VLLM_API_KEY="$(cat "$task_token_path")"
fi
exec "$task_env/bin/vllm" serve "$task_root/models/speech/Fun-CosyVoice3-0.5B-2512/omni-overlay" \
  --omni --host 127.0.0.1 --port 18869 \
  --deploy-config "$task_bundle/cosyvoice3_12gb_candidate.yaml" \
  --served-model-name tutor-cosy-tts
