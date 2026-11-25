# Local managed voice program deployment

`scripts/install-rust.ps1` stages seven reviewed voice program assets and three recognition program assets alongside the existing four executables and frontend build:

| Installed under `bin/local-speech/` | Purpose |
| --- | --- |
| `managed_tts.py` | Own one Linux service group and its Stop/EOF lifecycle |
| `tts_gpu_lease.sh` | Shared same-user/distribution project GPU lease |
| `cuda_link_env.sh` | Pinned Python 3.12 / CUDA 13 linker environment |
| `start_qwen_omni.sh` | Qwen engine launcher |
| `start_cosy_omni.sh` | Cosy engine launcher |
| `qwen3_tts_12gb.yaml` | Qwen single-request deployment configuration |
| `cosyvoice3_12gb_candidate.yaml` | Cosy single-request deployment configuration |

The recognition bundle installs `scripts/speech-service/{managed_asr.py,asr_service.py,recognizer.py}` under `bin/local-asr/` with the same filenames. The Rust managed ASR owner uses this installed wrapper and its sibling service/recognizer. The interpreter, Zipformer model directory, `runtime/local-asr-policy.json`, existing external service descriptor and all credentials remain outside program replacement; this addition does not enable a production policy or start the service.

Every file is copied to a staging directory and SHA256-checked before the installed program is asked to stop. The existing replacement/backup/rollback transaction includes both bundles. A missing or locked asset rejects the update; earlier replaced binaries and assets are restored on replacement failure.

Normal shutdown has one shared 35-second deadline across the stop-request child and main service exit. This allows the managed wrapper's 20-second cleanup budget plus bounded bridge/service overhead. Timeout refuses the update without forcing either the service or an unrelated process to exit.

## Runtime path contract

The Rust managed owner uses only `<canonical project root>/bin/local-speech/managed_tts.py`, passing `--root <canonical WSL project root>`, the configured engine, and the bound instance nonce as separate arguments. It must not silently fall back to source-tree scripts.

The wrapper finds its launcher beside itself. Launchers find their lease helper, CUDA helper, and YAML beside themselves, while models and runtime paths use the explicitly supplied project root. Source-tree diagnostics remain compatible: omitting `--root` defaults to the existing project root two directories above the wrapper. The internal `AI_TUTOR_PROJECT_ROOT` environment value carries this root from wrapper to launcher; it is not a shell command.

## Dependencies deliberately outside program replacement

The installer does not copy, download, move or create TTS model weights, API keys, the GPU Python environment, its `runtime/speech-gpu/omni-env-path.txt` pointer, production speech policy, or reference audio/configuration. Existing files remain unchanged. The model paths remain:

- `models/speech/Qwen3-TTS-12Hz-0.6B-CustomVoice`
- `models/speech/Fun-CosyVoice3-0.5B-2512/omni-overlay`

The active environment must match the pinned Python 3.12 / CUDA 13 helper. Qwen still supports its existing project-relative environment fallback; Cosy requires the explicit environment pointer. Neither launcher installs or repairs missing dependencies. Production integration must report an actionable local-voice preparation failure and retain SAPI when a dependency is unavailable.

The agreed managed policy uses explicit `managed.engine`, `managed.distribution`, and, for Cosy, `managed.references_file`. A stable reference file can be `runtime/tts-references.json`, with `zh`/`en` entries containing `wav_path`, exact `text`, and `sha256`; relative WAV paths resolve against that reference file's directory. The dated diagnostic manifest is not a production dependency. Creating that user-owned configuration is a separate explicit setup step, not an installer side effect. Managed credentials remain in memory/bootstrap stdin and do not belong in this bundle.

The lease is scoped to one Linux user/distribution and the fixed project TTS ports. These assets do not claim global coordination across every WSL distribution or unrelated GPU applications.

Wrapper failure events distinguish the failed operation from cleanup evidence. `failed` includes `linux_cleanup_confirmed: true` and an explicit boolean `process_started` only when no child was created (including lease/port preflight rejection) or the owned group's cleanup completed successfully. A `cleanup_failed` event explicitly reports `linux_cleanup_confirmed: false`; it never grants restart permission. Consumers must validate the bound instance and these fields, rather than treating a generic failure or bridge exit as proof that the GPU service was cleaned up. A preflight rejection never stops the foreign process that holds a port or lease.

## Validation

The PowerShell installation fixture uses only temporary files and fake stop processes. It checks seven voice and three recognition assets, exact ASR source/destination paths, backups, binary/voice/recognition rollback, unchanged credentials/models/environments/policy/reference configuration, missing-asset rejection before stopping, and the shared bounded stop deadline. Linux ownership tests exercise real sleeping fixture groups, plus relocated real launchers with fake helpers and an argv-printing executable; no models, CUDA or audio devices are used. The Windows managed diagnostic parser/cleanup tests remain separate from actual model startup validation.

The ASR bundle extension passed the existing PowerShell fixture on 2026-11-04; log: `runtime/goal-baselines/20261104/local-asr-installer-test.log`. Each of its three missing source files fails before the normal-stop gate. A locked final recognition asset rolls back earlier native, TTS and ASR replacements in the same transaction. This is an isolated fixture result, not a production installation or real ASR service test.

2026-11-04 implementation validation passed: the PowerShell installation fixture, 16 Linux ownership/deployment tests (the existing 14 plus two bundle path regressions), and nine Windows diagnostic tests. Deployment fixtures include paths containing spaces. No current installation was published, and no production speech policy/reference file, credential or model file was created or changed. The earlier real managed Cosy diagnostic validates the pre-bundle source wrapper; a real installed-bundle run remains the root agent's separate verification step.

The subsequent failure-proof change passed all 18 Linux ownership/deployment tests. Its two added regressions exercise actual temporary lease/listener contention without launching a service, and a synthetic control failure after launching an owned sleeping child. They verify explicit cleanup evidence, no contact with a foreign listener, retention of a foreign owner's lease, and disappearance of the owned process group before confirmed cleanup is emitted.

## Actual main-process bundle validation

The frozen seventh main binary (`99f7efefadecb1c51cd6d406a3deea833236d47791b7cdcac71d64bf74112764`) subsequently ran both real models from an isolated fixture's installed `bin/local-speech` bundle. Reports are `runtime/goal-baselines/20261104/main-managed-cosy-first/report.json` and `main-managed-qwen-first/report.json`. Each run reached active only after bilingual warmup and confirmed the actual AudioService client, rejected duplicate Retry while running, deactivated the client before owned cleanup, then retried with a fresh instance and canceled during startup. Both main processes exited normally, removed their descriptors, and left no enabled fixture policy. No production installation, microphone, speaker, camera or PTZ was used.

With a fresh per-fixture compilation cache, Cosy first became active after 203.292 seconds; Qwen after 227.184 seconds. These one-off cold preparations are still too slow for an immediate first-use experience. Qwen was ready at 222.892 seconds and spent 4.380 seconds warming: Chinese first PCM 2720.225 ms, then English first PCM 24.441 ms. These sequential warmup samples are not a balanced latency distribution or speaker-onset measurements. Cosy's corresponding warmup took 67.081 seconds, dominated by its first Chinese request. Earlier warmed synthesis baselines remain separate.

Owned service cleanup took 5.193 seconds for Cosy (forced group termination was needed) and 527.374 ms for Qwen (service exit code 0, no forced group termination). These measure complete model-service teardown, not key-to-silence. Each second instance was stopped before readiness and never installed a client. The actual preflight-conflict run in `main-managed-preflight-first/report.json` also confirmed rejection without contacting or terminating the occupied listener, followed by successful explicit retry and startup cancellation.

### Isolated Qwen startup candidate

`main-managed-qwen-eager-first` changes only the copied fixture configuration to `enforce_eager: true` in both stages. Its method was saved before model startup; base YAML SHA256 is `a79c0b71f391363df7a3dde0c4d5d6386315dfde997f7940449c96ddb451b9ea`, candidate SHA256 `3b44527b8d33d311fda428efcc7d45937462bb47b286325afcf25908775661c4`. The source/installed YAML and frozen seventh bundle remain unchanged.

The same main-process lifecycle checks passed, with readiness observed at 65.736 seconds and active at 81.067 seconds in a new fixture cache. Warmup cost rose to 15.287 seconds: Chinese first PCM 11151.065 ms and complete PCM 13760.983 ms; English first PCM 52.253 ms and complete PCM 1524.749 ms. Cleanup took 495.637 ms and the main exited normally. This single sequential comparison suggests a startup/steady-generation tradeoff; it is not a controlled repeated benchmark, a per-stage causal attribution, or listening evidence. Different generated sample counts and shared OS/GPU caches also prevent treating absolute full-generation times as matched output comparisons. The candidate is not promoted. The next decision requires the same fixed warmed corpus and resource/timing measurements under both configurations before choosing a deployment preset.

### Restart preflight correction

The first ABBA attempt completed the graph run, then the eager run failed before model readiness. Its old diagnostic projection omitted the failure's cleanup proof, so the exact original rejection reason cannot be recovered from that report; the incomplete result is retained. A separate no-model Ubuntu TCP reproduction showed that a plain bind probe rejects the server's remaining TIME_WAIT socket with errno 98 even after the listener has closed. A reuse-address probe accepted that completed connection's port while still rejecting a live listener. Python documents this POSIX TIME_WAIT use of [`SO_REUSEADDR`](https://docs.python.org/3.12/library/socket.html).

Both the managed wrapper and shared lab lease helper now use SO_REUSEADDR for their temporary TCP bind check, without SO_REUSEPORT or connecting to the occupied listener. The actual service must still bind its own socket and hold the shared GPU lease. `tts-timewait-linux-first.log` passed 19 real Linux ownership/fake-process tests, including immediate restart after an actual TCP connection and preservation of a listening socket that itself uses SO_REUSEADDR. `managed-windows-proof-tests-second.log` passed 10 diagnostic tests; a bound failed event's explicit cleanup evidence is now retained, while a failed preparation remains a failed run. Missing proof, a cleanup-failed event or a forcibly terminated bridge never proves Linux cleanup.

The second ABBA attempt uses fresh program-bundle copies containing this correction for both arms. It retains each arm's existing synthetic model/cache root, fixed 12-sentence corpus, two separately reported warmups and per-sentence request seeds. Original fixture assets, reports and the frozen seventh/eighth releases are unchanged. Its complete timing results remain separate from the first incomplete attempt.

### Completed warmed ABBA comparison

`runtime/goal-baselines/20261104/qwen-execution-abba-second/report.json` completed graph/eager/eager/graph successfully, with method SHA256 `2600aeb2c4aeee977e44f2a936bcf958969272f5498cfa38b035db04572a5831`. Each run performed two excluded warmups, then the same 12 sentences; each arm therefore has 24 measured requests, including 12 per language. All four services exited with code 0 and confirmed owned-group cleanup, without forced group termination. The failed first attempt remains separate.

| Warmed HTTP measurement | Graph | Eager |
| --- | ---: | ---: |
| Chinese first PCM, sample P50 / P95 | 44.0 / 52.3 ms | 72.2 / 87.1 ms |
| English first PCM, sample P50 / P95 | 42.8 / 54.4 ms | 65.4 / 91.5 ms |
| Largest generation-time / audio-duration ratio | 0.166 | 0.568 |
| Largest observed gap between received PCM chunks | 103.4 ms | 419.2 ms |
| Peak sampled total GPU memory, two runs | 7789 / 7789 MiB | 6932 / 6933 MiB |
| Service readiness, existing caches, two runs | 91.266 / 92.140 s | 43.843 / 42.454 s |

These are descriptive nearest-rank percentiles from a small repeated synthetic corpus, not population latency guarantees. GPU memory includes other desktop use. Existing per-arm caches, shared operating-system caches and generated duration differences remain confounders; fixed request seeds do not guarantee identical waveforms across kernels. There was no simultaneous camera/teaching workload, speaker output or listening evaluation. First PCM can include leading silence and is not the first audible word.

The current graph configuration remains the candidate for a warmed, persistent service: it had lower first-PCM delay and smoother continuous generation here. Eager reduced observed preparation time and sampled memory, but is not promoted based on that advantage alone. Neither mode has yet passed naturalness or actual speaker-onset acceptance, and neither is enabled in the installed production configuration.
