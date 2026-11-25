#!/usr/bin/env bash
# Local lab service. Does not select a production voice or play audio.
set -euo pipefail
task_bundle="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
task_root="$(cd "${AI_TUTOR_PROJECT_ROOT:-$task_bundle/../..}" && pwd)"
task_env="$task_root/runtime/speech-gpu/omni"
if [[ -f "$task_root/runtime/speech-gpu/omni-env-path.txt" ]]; then
  IFS= read -r task_env < "$task_root/runtime/speech-gpu/omni-env-path.txt"
fi
export PATH="$task_env/bin:$PATH"
export PYTHONUNBUFFERED=1
source "$task_bundle/tts_gpu_lease.sh"
tutor_tts_gpu_lease "$task_env/bin/python"
source "$task_bundle/cuda_link_env.sh"
export HF_HOME="$task_root/models/speech/hf-cache"
export XDG_CACHE_HOME="$task_root/runtime/speech-gpu/cache"
export HF_HUB_OFFLINE=1
export VLLM_NO_USAGE_STATS=1
export DO_NOT_TRACK=1
export TOKENIZERS_PARALLELISM=false
if [[ -n "${AI_TUTOR_MANAGED_TTS_TOKEN:-}" ]]; then
  export VLLM_API_KEY="$AI_TUTOR_MANAGED_TTS_TOKEN"
  unset AI_TUTOR_MANAGED_TTS_TOKEN
else
  task_token_path="$task_root/runtime/speech-gpu/omni-api-key"
  if [[ ! -f "$task_token_path" ]]; then
    "$task_env/bin/python" -c 'import secrets,sys; from pathlib import Path; Path(sys.argv[1]).write_text(secrets.token_urlsafe(32))' "$task_token_path"
  fi
  export VLLM_API_KEY="$(cat "$task_token_path")"
fi
exec "$task_env/bin/vllm" serve "$task_root/models/speech/Qwen3-TTS-12Hz-0.6B-CustomVoice" \
  --omni --host 127.0.0.1 --port 18868 \
  --deploy-config "$task_bundle/qwen3_tts_12gb.yaml" \
  --served-model-name tutor-qwen-tts
