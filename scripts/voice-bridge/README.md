# Voice bridge demo root

Run with Python 3.10+ on Windows:

```powershell
python scripts/voice-bridge/prepare_demo.py --source-root "C:\Users\RBMS\Desktop\AI TUTOR" --voice-root "C:\Users\RBMS\.cache\ai-tutor-voice-demo-01"
```

The destination must be new and on the same volume as the source models. The
script copies 12 necessary Qwen TTS/final-ASR helper files, verifies model files
against the existing acquisition manifests, and hardlinks only the 21 runtime
model assets. It creates new `synthetic_bilingual` TTS, enabled final-ASR, and
`qwen_pair_12gb_v1` policies for the same WSL distribution. Source policy files,
databases, credentials, user data, CPU ASR, and CosyVoice assets are not copied.

Pass the resulting directory to the separately built `ai-tutor-voice-bridge.exe`
with `--root "C:\Users\RBMS\.cache\ai-tutor-voice-demo-01"`. Preparation does not start it, invoke WSL, download anything,
load models, open audio devices, or verify that the existing Linux environments
are runnable. Model checksum reads can take time; no model bytes are duplicated.

Defaults reuse the existing `Ubuntu` distribution, ASR Python
`/mnt/c/Users/RBMS/.cache/ai-tutor-asr-17b-env-20261106/bin/python`, ASR cache
`/mnt/c/Users/RBMS/.cache/ai-tutor-asr-17b-runtime`, and the TTS environment pointer
at `source-root/runtime/speech-gpu/omni-env-path.txt`. Override with
`--distribution`, `--final-asr-python`, `--final-asr-cache`, and
`--tts-environment`. The ASR cache must remain under `/mnt/c/` with an `ai-tutor-`
basename, as required by the existing owner. The environments and cache are
referenced, not copied or changed by preparation.

Treat linked model assets as read-only: changing their contents changes the
source too. The script never deletes anything. A failure retains any partial
new directory; choose a fresh destination for a later attempt.

## Current stdio protocol

The source of truth is `rust/ai-tutor/src/voice_bridge/{mod,jobs,media,backend}.rs`.
Own one child process with redirected stdin/stdout and keep both pipes open.
Send UTF-8 JSON objects, one per line; continuously consume JSONL stdout. No TCP
bridge listener is added. Starting the EXE emits `hello` capabilities and starts
no model. Only an explicit `prepare` loads the configured owned TTS/final-ASR
services. This helper never captures a microphone; the caller supplies audio.

Every request has `version:1`, a positive integer `sequence` strictly increasing
across the whole helper session, and `id` (1–80 characters matching
`[A-Za-z0-9_.:-]`). Use a fresh ID for each request and correlate replies by
both ID and sequence. Request replies echo these fields with `event` and `data`;
reply sequences are correlation values, not a separate event counter. Initial
`hello`, final `shutdown`, and malformed-request `rejected` events have no
request ID/sequence. Input frames are limited to 1,500,000 bytes.

| `op` | Additional request fields | Behavior |
| --- | --- | --- |
| `status` | None | Returns backend state, current scope and active operation; starts nothing. |
| `bind` | `scope:{"owner":"opaque-view-id","generation":1}` | Establishes the media scope. A different binding requires a higher generation across the entire process, including owner switches; repeating the identical scope is harmless. Switching scope cancels old scoped work. |
| `prepare` | None | Returns `accepted`, then `completed` or `failed`; wait for readiness before media. Does not require a binding. |
| `speak` | `scope`, `text`, `language` | Speaks through the bridge-owned output device. Language is `zh`, `zh-CN`, `en` or `en-US`; speed is fixed to `normal`, with no request speed field. |
| `transcribe` | `scope`, `wav_base64` | Transcribes a complete WAV encoded with standard Base64. Language detection is automatic; no language hint field. |
| `cancel` | `operation_id`, `operation_sequence` | Targets the exact active prepare/speak/transcribe request, using that request's ID and sequence. Returns `cancel_requested`; it is not the operation's terminal event. |
| `shutdown` | None | Stops owned work/services and emits a final `shutdown` cleanup result. EOF also requests owned cleanup. |

For example, after binding and completing preparation:

```json
{"version":1,"sequence":4,"id":"speech-1","op":"speak","scope":{"owner":"view-a","generation":1},"text":"你好。Hello.","language":"zh"}
{"version":1,"sequence":5,"id":"cancel-1","op":"cancel","operation_id":"speech-1","operation_sequence":4}
```

Only one prepare/media job is active at once; another gets `voice_busy`.
`status`, `bind`, `cancel` and `shutdown` remain available while it runs. A
stale scope or replayed sequence is rejected. An accepted request sequence is
consumed even if its command is subsequently rejected; do not reuse it.

TTS accepts nonblank text up to **2,000 UTF-16 units**, split internally at
punctuation/whitespace or Unicode boundaries into at most 320 characters per
chunk. `speech_started` contains `chunk_index` and an actual output receipt
(`playback_id`, `epoch`, `engine_kind`, `milestone`). `accepted` never means
speech has started. Completion contains the per-chunk outcome receipts; use
their actual engine because the existing output path permits SAPI fallback
before first output. Neither start/completion nor cancellation is an acoustic
measurement: `speaker_output_measured` remains false.

ASR accepts RIFF/WAVE **PCM signed 16-bit little-endian, mono, 16 kHz**, with
nonempty PCM up to **640,000 bytes / 20 seconds** and a WAV container up to 1 MiB.
Malformed/duplicate chunks, inconsistent RIFF lengths and unsupported formats
are rejected. `completed.data` includes `text`, `detected_language`, `elapsed_ms`,
`provider`, `final:true` and scope. Audio remains in memory; no WAV path is sent.

Terminal operation events are `completed`, `failed`, or `canceled` (one “l”).
`canceled` revokes delivery and includes `stop_error`; a non-null error must stay
visible as unconfirmed stopping, not be presented as silence. Fence late results
immediately on local cancellation/scope changes and wait for the operation to
retire before a replacement. Check final `shutdown.data.cleanup_confirmed` and
child exit; merely closing the window is not cleanup evidence. Do not persist
raw request/response streams containing speech text or audio.

## Destiny connection remains pending

The C# consumer adapter is now implemented in
[`integrations/destiny-voice`](../../integrations/destiny-voice/README.md), with
18 passing model-free consumer checks and an actual C# → Rust → local Qwen
bilingual ASR/playback/cancel/shutdown run. A DLL and source handoff are available
under `runtime/goal-releases/20261109-destiny-voice-adapter`. This establishes the
private process connection; it has not yet been wired into Destiny's window or
Voice port, and it does not add microphone capture or automatic interruption.

Destiny is **not yet connected** to this helper. Its Native/UI paths currently
belong to `core_delivery`; coordinate the precise source lease and frozen voice
capability successor with that owner before changing them. Keep Destiny's
existing UI, authentication, companion/persona, Chat and Memory authorities.

The small connection fits behind `apps/desktop-windows/DesktopBridge.cs`: own
the Rust child there (or in one private C# adapter), translate existing
`WindowsSpeechOutput` play/stop into `speak`/targeted `cancel`, and translate
`WindowsSpeechInput.TranscribeClosedWave` into in-memory WAV Base64/transcribe.
Retain native hold capture, device closure before transcription, editable
transcript staging and separate explicit Chat Send. Map each current full
Destiny binding to an opaque helper owner and monotonically increasing helper
generation; these are correlation scopes, never authentication credentials.
Keep the native source/nonce/sequence checks and local stale-operation fences.
Logout, release and navigation cancel/fence current operations; re-enable must
revalidate through the existing Destiny path and bind freshly. Host exit owns
helper shutdown.

The TS connection continues through `src/modules/chat/ports/companionVoice.ts`
and `adapters/browser/desktopVoiceBridge.ts`. Add an accurate local-bridge
capability identifier through the existing contract process rather than label
Rust as `windows-system-speech`. Update the corresponding input-instruction
check in `ui/CompanionVoiceControls.tsx`; other UI consumers can keep their
existing state shapes. Translate actual `speech_started`/terminal receipts and
ASR text into that port; do not expose helper pipes, raw WAV or account data to
the renderer, add another voice controller, or bypass the normal Send path.

Two existing limits need an explicit integration decision: Destiny captures up
to **30 seconds / 960,000 PCM bytes**, while this helper accepts **20 seconds /
640,000 bytes**; cap capture at 20 seconds or reject longer input without silent
truncation. Destiny's System.Speech stop deadline is **1.2 seconds**, while the
current Rust `AudioService::stop_speech` can wait **4 seconds** for its stop
receipt. Preserve immediate delivery fencing but reconcile the confirmation
deadline and visible error behavior before claiming compatible stopping. Those
timeouts describe software receipts, not measured speaker-stop latency.
