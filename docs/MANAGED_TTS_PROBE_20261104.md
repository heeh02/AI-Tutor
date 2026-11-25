# Managed TTS Windows diagnostic

This tool is an explicit synthetic diagnostic, separate from production voice configuration. It owns one `wsl.exe -d Ubuntu` bridge and the matching `managed_tts.py` instance. It does not select a voice, create `AudioService`, open input/output devices, or stop any existing lab service. Arrange exclusive GPU use before running it; an occupied project TTS port makes the Linux wrapper refuse startup.

## Invocation

Build the Rust release probe first, then run from Windows with a **new output directory**:

```powershell
python scripts/speech-lab/probe_managed_tts.py --real-local-synthetic --root "C:\Users\RBMS\Desktop\AI TUTOR" --engine cosy --output "C:\Users\RBMS\Desktop\AI TUTOR\runtime\goal-baselines\20261104\managed-cosy-first"
```

`--engine qwen` uses port 18868 and `tutor-qwen-tts`; `cosy` uses 18869 and `tutor-cosy-tts`. No model/dependency download is performed. The wrapper uses the existing project launcher and its existing GPU environment.

## What the report proves

- `starting`: a protocol-v1 event with this invocation's instance nonce and the expected engine/model/loopback endpoint. It explicitly does not establish readiness or warming.
- `ready`: within a shared 240-second starting/readiness budget, `/v1/models` accepted this invocation's newly generated credential and listed the expected model; the same request **without** a credential returned HTTP 401. No prewarm is inferred from HTTP readiness.
- `warm`: the Rust `tts-stream-probe.exe --prepare-bilingual` process exited successfully and returned both validated Chinese and English synthetic PCM preparation receipts. This drains PCM without playback and reports pending installation, never an installed production client. The Rust probe has its own 120-second total deadline; its Windows process has a 150-second outer limit.
- `stopped` / `linux_cleanup_confirmed`: the matching Linux wrapper reported `stopped` and the bridge exited. An unexpected service exit is still a failed diagnostic even when cleanup is confirmed. Normal diagnostic completion accepts owner Stop/EOF only.

Every error returns exit code 1. `prepare_exit_code` retains the actual Rust process exit code. Wrapper events, preparation output and stderr logs are retained, with the fresh credential redacted before persistence. Startup, readiness, warm preparation, and shutdown can therefore be assessed separately.

## Ownership and cleanup

The fresh random credential travels to WSL only in the first stdin JSON line, never argv. The bridge stdin stays open throughout preparation. Its stdout is continuously consumed by a JSON reader thread and stderr by a persistent, credential-redacting log reader. Stderr persistence is capped at 32 MiB; reaching the cap or a disk failure makes the diagnostic fail while the reader continues draining to permit shutdown.

The tool creates `output/fixture`, writes only its candidate key there, and copies the Cosy reference manifest verbatim when needed. Reference WAV paths remain the original absolute Windows paths; neither reference audio nor the production key is modified. Rust creates and normally cleans its own nested diagnostic descriptor.

Finally, the tool sends `{"op":"stop","instance_id":"<this nonce>"}`, flushes, closes stdin as an EOF fallback, and waits up to 20 seconds for shutdown. If that bridge times out, only this `Popen` bridge is terminated; Linux cleanup is explicitly **unconfirmed**, even if the Windows bridge then exits. There is no distribution shutdown/termination, PID discovery or process-name kill.

The tool deletes its exact candidate key and any matching credential-bearing Rust descriptor left under its exclusively created fixture. It checks ownership and contents before deletion, leaves all other fixture files/directories intact, and records cleanup failure. It does not recursively delete the fixture.

## Validation boundary

`test_probe_managed_tts.py` exercises protocol ownership, endpoint binding, preparation evidence, exact credential cleanup and shutdown receipt interpretation without WSL, network, models or devices. Implementation review and offline tests do not establish real model startup/cleanup; the root agent performs that explicit diagnostic after review and after arranging service exclusivity.

2026-11-04 implementation validation: nine offline tests passed with `python -B -m unittest discover -s scripts/speech-lab -p test_probe_managed_tts.py -v`. Real managed startup/readiness/preparation/shutdown remains untested by this implementation agent.

## Root real-model validation, 2026-11-04 03:29

`runtime/goal-baselines/20261104/managed-cosy-first/report.json` completed successfully against a newly owned Cosy instance. Readiness took 89.719 seconds; credential-authenticated model discovery returned 200 and unauthenticated discovery returned 401. Rust bilingual preparation then took 7.245 seconds: Chinese first PCM 4.828 seconds and English 639 ms, with 65,280 and 58,560 samples respectively. This cold instance preparation is separate from the earlier already-warm external-service measurement.

Owner Stop required forced group termination after the grace period (`service_exit_code=-9`, `forced_group_stop=true`). The wrapper confirmed no live owned-group processes remained, the Windows bridge exited 0, and exact credential cleanup completed. Total diagnostic duration was 102.203 seconds; post-run GPU usage returned to 1,751 MiB. Thus lifecycle cleanup passed, while graceful model shutdown was not demonstrated. No production client was installed, and no microphone or speaker was opened.
