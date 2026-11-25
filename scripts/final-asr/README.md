# Independent Qwen offline-final bundle

Install these **six files together**, byte-for-byte, into `root/bin/final-asr/`:

- `managed_final_asr.py`
- `qwen_final_engine.py`
- `qwen_final_service.py`
- `qwen-model.json`
- `qwen-sdk.json`
- `gpu_admission.py` (the shared source is `scripts/speech-lab/gpu_admission.py`)

The engine uses its bundle directory for both fixed manifests. There is no
dependency on dated baseline directories. The model stays at
`root/models/speech/Qwen3-ASR-1.7B-7278e1e7`; the two manifest SHA-256 values remain
`a1aeda7dd900d4cafeca85f1c166fcf26e6b459a4401255afa602720b1a9ec83` and
`17167049ae5bd88c0988895967080a4129716a0aabc953dd1ecd588ea8447121`.

The Linux owner is invoked with separate argv elements:

```text
python3 managed_final_asr.py --root <absolute-Linux-project-root> --instance <fresh-32-hex-id> --python <absolute-independent-venv/bin/python> --cache-dir <absolute-task-owned-C-cache>
```

`--cache-dir` must resolve under `/mnt/c/` and have a basename beginning with
`ai-tutor-`; the current independently prepared cache is
`/mnt/c/Users/RBMS/.cache/ai-tutor-asr-17b-runtime`. The owner derives CUDA library
paths from the supplied Python 3.12 environment, removes inherited TTS/Python
overrides, creates only those explicit cache subdirectories, and starts the
service directly with argv and `--port 0`. No shell launcher is needed.

Within five seconds, private stdin receives exactly
`{"protocol_version":1,"instance_id":"<same-id>","bearer_token":"<private-token>"}`.
Keep stdin open. With no GPU policy, the owner acquires the existing TTS GPU-0
lease exclusively. An explicit `runtime/gpu-resource-policy.json` containing
`{"protocol_version":1,"mode":"qwen_pair_12gb_v1"}` permits the verified
Qwen1.7B ASR and fixed Qwen0.6B TTS profile to reside together. Both enabled
policies must name the same WSL distribution/default user; TTS must use managed
Qwen with synthetic bilingual warmup. This profile uses a shared global lease,
exclusive per-role leases and an exclusive initialization lease. Preparation is
serialized; readiness releases only initialization ownership. Global and role
fds are inherited by the exact new-session child, so a bridge/owner failure
alone does not silently release a live service's lease. Stop both services
before changing the GPU profile, distribution or default WSL user.

The shared profile may first emit one cancellable `waiting_gpu` without a
child PID. The owner emits `starting` with verified `pid` and `process_group`, then
validates and forwards `awaiting_owner`, `loading`, `warming`, `warm_ready`.
Only `warm_ready` has `ready=true` and `warmup_completed=true`; model-ready is a
separate field. Child `failed`, `cleanup_pending`, and `stopped` may also pass
through, but none proves owner cleanup. Only owner `starting` contains pid/group.

An exact `{"op":"stop","instance_id":"<same-id>"}` line, stdin EOF, or SIGINT/
SIGTERM initiates retirement. Stale-instance Stop is ignored. The owner first
forwards exact Stop and permits ten seconds for real worker retirement; then
only its owned process group receives SIGTERM (up to five seconds), followed
when necessary by SIGKILL (up to five seconds). A final child wait is bounded
by five seconds. No process-name search or unrelated process kill is used.

Successful cleanup emits `owned_stopped` with `reason`, `forced_group_stop`,
`service_exit_code`, `process_started`, and `linux_cleanup_confirmed=true`.
Before-child failures use `reason=startup_failed`, `process_started=false`,
`service_exit_code=1`. Unconfirmed group cleanup emits `cleanup_failed` with
`linux_cleanup_confirmed=false`. The Rust owner still must observe the WSL
bridge exit. Reasons are `owner_stop`, `owner_eof`, `owner_signal`,
`service_exited`, `startup_failed`, `control_error`, `protocol_error`, `owner_error`.

The service retains one-worker admission, cancellation invalidation, actual
warmup, raw decoder metadata, and 502 `decoder_truncated` rejection. A cancellation
callback is also checked between generated tokens; it cannot preempt a running
CUDA kernel, prefill or compilation. Busy remains true until worker retirement
and synchronization. Failed synchronization requires process exit before
reclaiming GPU ownership. This bundle is offline-final ASR, not Qwen streaming.
Controlled co-residency and real Workstation lifecycle results are recorded in
`docs/QWEN_GPU_PAIR_20261106.md`; they do not establish microphone or acoustic
latency acceptance.

Root performs verification; the author did not run these tests or services:

```powershell
runtime/speech-lab/.venv/Scripts/python.exe -B -m unittest discover -s scripts/final-asr -p test_*.py -v
```

The copied core fake tests and new owner tests use no real child, socket bind,
GPU, model, audio device or cache writes. Owner tests cover private bootstrap,
ordered events, rejection of forged pid/cleanup fields, exact/stale Stop,
group escalation and cleanup failure, six-file installation, path spaces,
and independent cache/CUDA environment construction.
