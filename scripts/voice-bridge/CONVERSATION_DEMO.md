# Separate AI Tutor conversation Demo

`prepare_conversation_demo.py` prepares a new directory using existing local
assets. It does not start a service, download anything, open devices, read an API
key, copy a database, or change the installed application or its policies.
Python 3.10+ and the already prepared Windows/WSL inference environments are
required. The new root must be on the source model volume for hardlinks.

From the AI Tutor checkout, choose an unused destination and explicitly select
the built main executable and UI:

```powershell
$src = 'C:\Users\RBMS\Desktop\AI TUTOR'
$demo = 'C:\Users\RBMS\.cache\ai-tutor-conversation-demo-20261109'
& "$src\.venv\Scripts\python.exe" "$src\scripts\voice-bridge\prepare_conversation_demo.py" `
  --source-root $src --demo-root $demo `
  --executable "$src\target\release\ai-tutor.exe" `
  --ui-dir "$src\runtime\goal-baselines\20261109\conversation-layout-check\ui-dist"
```

The UI path above is the staged five-file UI with the verified conversation
layout and current 2.5-second endpoint notice; the caller must use a
build suitable for the selected main executable. The script checks required
files, not whether a build has passed runtime acceptance. Use the current main
build containing managed Qwen ownership and Flash metadata streaming.

The script reuses the voice-root preparer, then adds the main program, five UI
files, teaching prompts/schema, the nonsecret Kimi verification receipt, CPU
Zipformer partial-ASR helpers and four pinned model assets. Cached Whisper base
is included as a verified hardlink when available; use `--no-whisper-fallback`
to omit it. `--with-camera` adds the three cached face/ONNX assets for later
manual opening. Neither camera selection nor PTZ configuration is copied.

The new policies select local Qwen final ASR/TTS with `qwen_pair_12gb_v1`, local
Zipformer partial captions, Flash `non_thinking`, and segment playback after
display acknowledgement. No background-memory policy is created. Existing
foreground learning records still work; enabling background shadow processing
is a separate choice. Databases are created fresh by the main program on first
launch. No original learner records, configuration, cloud credential, raw audio
or private service descriptor is copied.

Defaults reference the existing `Ubuntu` distribution, the ASR environment and
cache described in [the voice bridge README](README.md), and the source TTS
environment pointer. Optional `--partial-asr-python`, `--distribution`,
`--final-asr-python`, `--final-asr-cache`, and `--tts-environment` select other
already installed environments. Starting the Demo uses the current Windows
user's existing DeepSeek DPAPI credential and Kimi installation/login; keys
remain outside the Demo root.

After preparation, choose an available local port and launch:

```powershell
Start-Process -FilePath "$demo\bin\ai-tutor.exe" `
  -ArgumentList @('--root', ('"' + $demo + '"'), '--port', '8877') `
  -WindowStyle Hidden
```

This opens the authenticated workbench and starts the explicitly configured
local model preparation. Cold preparation can take several minutes; readiness
is displayed in the existing UI. Create a learner profile, select a microphone,
and wait for recognition and voice readiness. Use recording with editable final
text and explicit Send first; automatic voice rounds remain an explicit UI
choice. Keep this main process as the single owner of the local Qwen pair; the
standalone `ai-tutor-voice-bridge.exe` is a separate alternative for other hosts.

Normal stop:

```powershell
& "$demo\bin\ai-tutor.exe" --stop --root $demo
```

The stop command acknowledges the shutdown request; allow the original main
process to finish releasing its owned services before starting another owner.
The existing main `--prepare-demo` command is unrelated to directory staging:
it creates an adult profile in the real namespace and opens the camera. Normal
startup and the workbench are sufficient for this conversation Demo.

This is the existing modular-rover teaching application, not a Destiny UI or a
general-subject tutor. The package preparation itself is not evidence of real
microphone, visible-browser, acoustic-output or teaching-quality acceptance.
Treat linked model files as read-only. A preparation error retains a partial
new root; choose another fresh destination rather than overwriting it.
