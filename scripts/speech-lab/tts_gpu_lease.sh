#!/usr/bin/env bash
# Source this before any CUDA/model initialization. The fd survives exec.
# Optional path/ports exist for isolated ownership tests, never model selection.
tutor_tts_gpu_lease() {
  local task_python="${1:-python3}"
  local task_lease_path="${2:-/tmp/ai-tutor-tts-gpu-0-$(id -u).lock}"
  local task_qwen_port="${3:-18868}" task_cosy_port="${4:-18869}"
  local task_lease_fd="${AI_TUTOR_TTS_LEASE_FD:-}" task_opened=0
  local task_admission_mode="${AI_TUTOR_GPU_ADMISSION_MODE:-exclusive}"
  if [[ "$task_admission_mode" == qwen_pair_12gb_v1 ]]; then
    # Only an owner-admitted Qwen launcher may reuse SH global + EX role/init.
    # The helper checks all three inherited inode identities and never upgrades
    # the common global lock to EX. Raw/default launches keep the old EX path.
    local task_helper_dir task_caller
    task_helper_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    task_caller="${BASH_SOURCE[1]:-}"
    if [[ -z "${task_root:-}" || -z "$task_caller" ]]; then
      printf '%s\n' 'Qwen pair GPU admission requires its owned launcher context' >&2
      return 1
    fi
    "$task_python" "$task_helper_dir/gpu_admission.py" --validate-inherited \
      --root "$task_root" --profile "$task_helper_dir/qwen3_tts_12gb.yaml" --caller "$task_caller"
    return $?
  elif [[ "$task_admission_mode" != exclusive ]]; then
    printf '%s\n' 'Unsupported GPU admission mode' >&2
    return 1
  fi
  if [[ -z "$task_lease_fd" ]]; then
    local task_previous_umask
    task_previous_umask="$(umask)"
    umask 077
    if ! exec {task_lease_fd}>>"$task_lease_path"; then
      umask "$task_previous_umask"
      return 1
    fi
    umask "$task_previous_umask"
    task_opened=1
  elif [[ ! "$task_lease_fd" =~ ^[0-9]+$ ]]; then
    printf '%s\n' 'Invalid inherited TTS GPU lease descriptor' >&2
    return 1
  fi
  # flock locks the shared open-file description. The short checker may exit;
  # the calling shell and its exec child retain the locked descriptor.
  if ! "$task_python" - "$task_lease_fd" "$task_lease_path" "$task_qwen_port" "$task_cosy_port" <<'PY'
import fcntl
import os
import socket
import sys

try:
    fd = int(sys.argv[1])
    inherited, expected = os.fstat(fd), os.stat(sys.argv[2])
    if (inherited.st_dev, inherited.st_ino) != (expected.st_dev, expected.st_ino):
        raise ValueError("wrong lease file")
    fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
    for port in sys.argv[3:]:
        with socket.socket() as probe:
            probe.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            probe.bind(("127.0.0.1", int(port)))
except (OSError, ValueError):
    raise SystemExit("TTS GPU lease unavailable, invalid, or a project TTS port is already owned")
PY
  then
    if (( task_opened )); then exec {task_lease_fd}>&-; fi
    return 1
  fi
  export AI_TUTOR_TTS_LEASE_FD="$task_lease_fd"
}
