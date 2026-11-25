# Voice bridge consumer checks

The harness compiles the adapter and its consumer into a fresh evidence directory
with the installed .NET Framework compiler. It uses only BCL assemblies and
`System.Web.Extensions`; the project does not acquire packages.

The injected transport cases verify observable consumer behavior:

- `accepted` does not complete an operation or synthesize a speech-start event.
- `speech_started` preserves actual playback receipts and correlates both keys.
- Cancellation revokes delivery before I/O, targets the exact ID and sequence,
  and keeps the operation busy until its terminal response.
- Switching owner raises the generation and fences old starts/transcripts.
- EOF and transport failure settle pending operations without exposing payloads.
- An accepted prepare survives more than three seconds without a terminal reply.
- Hello and shutdown deadlines are bounded, and missing cleanup/exit evidence is
  reported as unconfirmed.
- Concurrent requests appear on the wire in strictly increasing sequence order.

An executable synthetic helper exercises the real process transport, including
stderr consumption and child exit. An empty synthetic root is used for the real
Rust release helper: hello, bind, status, media before prepare, and shutdown only.
No test sends prepare to the Rust helper, loads a model, captures microphone
input, launches Destiny, or reads user content, credentials, or databases.

Each invocation writes a new timestamped directory beneath
`runtime/goal-baselines/20261109/destiny-adapter-next/`. Evidence includes source
and binary hashes, compilation results, named assertion results, and exit status.
Raw protocol frames, synthetic transcript text, audio, and stderr bodies are not
persisted in evidence.
