# Local transcription preparation UI

The independent `rust/tutor-ui/src/local_asr_status.rs` card is mounted in the existing composer beside local voice status. Missing `state.local_asr`, missing phase and `disabled` remain hidden for older/unconfigured backends. Creating or refreshing the component performs no request and opens no device.

Chinese and English copy distinguishes starting, loading, warming, active, degraded, stopping, stopped, cleanup unconfirmed and configuration errors. Details show only the separate `ready`, `warmup_completed` and `active` flags. Active means the client is available for subsequent recordings, not that a microphone is open or that transcripts are accurate. No token, path, process identity, raw error or ownership metadata is rendered.

Explicit Retry is available only for stopped/degraded/configuration-error with `retry_available=true`, `busy=false` and `shutting_down=false`. Cleanup failure never offers Retry. Explicit Stop is available for a managed starting/loading/warming/active lifecycle even when that lifecycle reports busy. The component holds one request at a time; stopping and shutdown do not offer another Stop. The main Stop button and recording/draft state are unchanged.

Both actions POST an empty object to `/api/audio/local-asr/retry` or `/stop`. Stop has a dedicated 15-second UI budget for the backend's agreed 10-second cleanup bound. Completion requests the existing sequenced App state refresh; POST success alone is never merged into an active/stopped status. Errors use fixed bilingual uncertainty copy and do not automatically retry.

Unavailable/stopped copy says that text can still be edited and that offline transcription after recording requires a working microphone and local Whisper model. Cleanup failure offers text editing without claiming a fallback or client deactivation was verified.

Five pure logic tests cover hidden legacy/disabled state, separate preparation/installation milestones, explicit retry permissions, busy owned lifecycle stopping, and safe bilingual projection. `rustfmt` has passed. Root validation in `runtime/goal-baselines/20261104/workspace-twenty-third.log` passed all 615 workspace tests, including 109 UI tests, with four existing ignored tests and no warnings. `wasm-local-asr-first.log` built the staged frontend successfully. These checks do not validate browser/device interaction: the synthetic task fixture `rust/tutor-ui/fixtures/local-asr-status-20261104.json` still has all manual tasks marked `NOT_RUN`.

Backups are under `runtime/goal-backups/20261104/ui_review/local-asr-status/`. The same slice added three ASR program assets to the existing installer; its temporary-file/fake-process PowerShell test passed and is documented in `LOCAL_SPEECH_DEPLOYMENT_20261104.md`.
