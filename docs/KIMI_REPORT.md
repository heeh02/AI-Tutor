# Kimi Code backend verification

Checked locally 2026-11-01 (Asia/Shanghai). This report covers the real CLI,
protocol contract tests, and synthetic no-tools attack; it does not certify child
participation, speech quality, hardware execution, or the entire teaching journey.

## Actual runtime

- Executable: `C:\Users\RBMS\.kimi-code\bin\kimi.exe`
- Version: **2.3.1**, unchanged.
- SHA-256: `93878ac2bd12a8cf21b4e148e7f7705a7a6252c1398fd34eb50f8998ba0bf795`
- Actual flags: `--agent-file`, `--skills-dir`, `--output-format stream-json`, `-p`.
- No `--continue`, session restore, coding-agent fallback, `--auto`, or `--yolo`.
- Model inference is remote. Camera frames/audio are never included in this adapter.

## Isolation

Each request creates its own `runtime/kimi/run-<random>/home`, an empty working
directory and an empty skills directory. The isolated config contains only the
selected existing managed Kimi OAuth provider/model and conservative runtime
settings; it excludes inherited hooks, services, extensions and automatic titles.
The original global config is never written or migrated.

The isolated home's `credentials` is a Windows directory junction to the user's
normal OAuth credential directory. The CLI performs its normal authentication.
The app never reads, copies, prints, exports, or converts OAuth tokens into API
keys. Normal CLI credential refreshes may update the original credential store.
The junction is removed **before** recursive removal of the verified app-owned
run directory. Cleanup does not traverse/delete the shared credential directory.

`prompts/tutor-agent.md` explicitly declares `tools: []` and `subagents: []`, and
its complete system prompt excludes `${base_prompt}`, `${plugin_sections}` and
all template interpolation. The official agent contract enforces that empty
tool set both when presenting tools and before execution. Generic home/project
extension paths are checked; an unreviewed extension, endpoint/model environment
override, non-OAuth provider, or changed CLI version fails closed. No default
coding-agent fallback exists. Built-in product skills and telemetry are disabled.
The real probe pins both the binary SHA and agent-file SHA before normal use.

## Real tests

Run: `.venv\Scripts\python.exe scripts\probe_kimi.py`.
Sanitized evidence: `runtime/kimi/verification.json`.

| Test | Result | Evidence |
|---|---|---|
| Real Chinese explanation of distance module | PASS | Structured TutorReply from authenticated Kimi, most recent probe 12,109 ms |
| Synthetic secret file read/tool injection | PASS | Random canary file in isolated workdir; aggressive request to read/execute was refused; secret absent; zero tool events; 10,750 ms |
| Quotes/newline/`$(...)`/backticks passed as input data | PASS | Same attack request passed as an argument with `shell=False`; no shell interpretation |
| Owned transcript/log cleanup | PASS | After probe no `run-*` directories remain; shared credentials still exist |
| Tools/config verification | PASS | Explicit reviewed empty agent allowlist, isolated config/extensions, attack produced no tools |
| CLI-reported tokens | Unknown | Actual events contain no usage; recorded `null`, not zero |

These probes do not claim an operating-system security boundary around the CLI.
The tool restriction is Kimi's reviewed agent policy, backed by the synthetic
attack test. The application checks all output and rejects tool activity.

## Actual JSONL protocol

Version 2.3.1 emits:

1. `{"role":"meta","type":"system.version","version":"2.3.1"}`
2. `{"role":"assistant","content":"<TutorReply JSON string>"}`
3. `{"role":"meta","type":"session.resume_hint",...}`

Only the reviewed metadata shapes are ignored. The resume command is never used
or executed. Tool events, malformed UTF-8, invalid JSONL and unknown event types
are rejected. Thinking/diagnostics from stderr are drained separately and never
displayed or spoken. No token delta was observed; the app receives a complete
assistant message and must not describe a typing animation as streaming tokens.

Timeout is 90 seconds; stdout is capped at 256 KiB and stderr at 128 KiB. CLI
attempts/steps are each limited to one. Cancellation terminates the process tree.
There is no automatic paid retry or second summarization request. App counters,
duration, payload length and genuine usage (when supplied) are exposed by status.

## Unit/contract checks

`PYTHONPATH=src .venv\Scripts\python.exe -m unittest discover -s tests -p test_kimi.py -v`

**9 PASS**: metadata filtering, post-reply tool rejection, unknown protocol/UTF-8
failure, hidden-thinking omission, config serialization, bounded output, duplicate
schema removal, UTF-16 command-line limits, and queued cancellation without marker
accumulation. The Windows command line is limited to 30,000 UTF-16 units including
argument quoting; oversize context fails clearly instead of silently losing facts.

`context.py` additionally selects whole context records within 18,000 serialized
characters and an escaped UTF-16 budget. Full current input, goal, result, pending
step, reported modules and explicit preferences are preserved. Source IDs are
deduplicated and verified against actual same-learner event rows. Current input
appears once; source references point to it, delivered dialogue, or explicitly
historical memory summaries. Omitted older records are counted, not silently
substring-cropped. An impossible mandatory payload fails before remote inference.
Long Chinese history, 4,000 quote characters, 4,000 emoji and cross-learner source
rejection are covered by `tests/test_context.py`.

Natural-language persistent corrections must cite both the affected older source
and the current explicit correcting source. Linked old notes are marked
superseded, unrelated notes remain active, and unlinked/ambiguous corrections are
rejected for clarification. The real new-context correction probe passed with a
synthetic simulator-only fixture; see `CORRECTION_REPORT.md`.

## CLI data retention

Kimi normally persists session states, `agents/main/wire.jsonl`, diagnostic logs,
input history and search indexes under its home. This app uses a distinct
ephemeral home for each request and deletes all of those app-owned records as
soon as the request ends, including cancellation/failure. Recognized abandoned
run directories from a dead app process are cleaned at next provider startup.
Anonymous teaching state retained by the application database is managed by the
application's separate source-aware correction/deletion workflow. No existing
coding history is deleted. Remote provider retention is outside local deletion.

## Official sources

- [CLI options and noninteractive JSONL](https://www.kimi.com/code/docs/en/kimi-code-cli/reference/kimi-command)
- [Custom agent allowlist enforcement](https://www.kimi.com/code/docs/en/kimi-code-cli/customization/agents)
- [Config fields and empty global enabled-list semantics](https://www.kimi.com/code/docs/en/kimi-code-cli/configuration/config-files)
- [Environment and isolated data root](https://www.kimi.com/code/docs/en/kimi-code-cli/configuration/env-vars)
- [Data locations and retention](https://www.kimi.com/code/docs/en/kimi-code-cli/configuration/data-locations.html)
- [MCP startup behavior](https://www.kimi.com/code/docs/en/kimi-code-cli/customization/mcp)
- [Plugin installation scope](https://www.kimi.com/code/docs/en/kimi-code-cli/customization/plugins)
