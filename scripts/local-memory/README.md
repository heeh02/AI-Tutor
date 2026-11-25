# Owned local BGE-M3 worker

This bundle is a private JSONL child of the native AI Tutor owner. It does not
open an HTTP port, access a database, download assets, accept arbitrary commands,
or enable retrieval by itself. The native actor owns policy, request deadlines,
text selection and all decisions about using a vector.

`managed_embedding.py` runs under the selected Linux/WSL Python environment:

```text
python -u managed_embedding.py --instance <fresh 32 lowercase hex characters> --model-dir <absolute Linux directory>
```

The native owner resolves Windows paths with `wslpath` and supplies argv directly,
without a shell. The manager uses the same Python interpreter to start the
sibling `embedding_worker.py`. It reports `awaiting_owner` before starting any
worker. Only a valid private `start` command releases the loading gate.

The installed bundle needs these two Python files; the model directory must
contain every pinned file listed in `download_bge_m3.ASSETS`. The worker contains
the same immutable asset manifest and checks exact size plus the official LFS
SHA-256 or Git blob digest. A mismatch fails; there is no download or fallback.

## Encoding contract

- Model: `BAAI/bge-m3`, revision `5617a9f61b028005a4858fdac845db406aefb181`.
- Artifact: the pinned `pytorch_model.bin`, CPU float32, eager attention,
  `weights_only=True`, `use_safetensors=False`, `trust_remote_code=False`.
- One tokenizer/model load per worker lifetime. A fixed warm-up precedes ready.
- Two Torch intra-op threads, one inter-op thread, no visible CUDA devices.
- Hugging Face/Transformers/Datasets offline flags and telemetry disabled;
  `local_files_only=True` and no tokenizer fallback.
- Fast tokenizer, special tokens included, no padding or truncation. More than
  512 tokens is an explicit `input_too_long` error, with the actual token count.
- Query and document prefixes are both empty. The exact input bytes are hashed.
- First-token/CLS pooling, L2 normalization, 1,024 finite float32 dimensions.

The full `profile` object equals Rust's
`EmbeddingProfile::BgeM3NativeFp32.metadata()`, including model key, revision,
weight/tokenizer digests, pooling, empty prefixes, artifact, normalization and
maximum token count. A different profile is rejected rather than coerced.

## Private wire protocol

Every command and event is one UTF-8 JSON object followed by LF, at most 65,536
bytes **including** the LF. Protocol version is integer `1`. Duplicate fields,
nonfinite JSON values, missing/extra fields and wrong identities are rejected.
Only the native owner receives stdout. No raw input, exception message, native
library output, file path or model reasoning is copied into events. The worker's
ordinary stdout/stderr are discarded by the manager; validated JSONL uses a
separate saved stdout descriptor.

Every command contains `protocol_version`, `instance_id`, and `op`:

| op | Additional fields |
| --- | --- |
| `start` | `profile` (exact fixed object) |
| `encode` | `profile`, `request_id`, `role`, `text`, `input_sha256` |
| `stop` | none |

`request_id` is 1–96 ASCII letters/digits/`_`/`.`/`-`; `role` is `query` or
`document`. The hash is lowercase SHA-256 of the exact UTF-8 text. There is one
encode in flight; neither the manager nor worker automatically retries it.

Events contain `protocol_version`, `instance_id`, and `state`:

| state | Important fields |
| --- | --- |
| `awaiting_owner` | `manager_pid`, `ready:false`, `warmup_completed:false` |
| `starting` | worker `pid`, `process_group`, false ready/warm-up flags |
| `loading` | `profile`, false ready/warm-up flags |
| `ready` | `profile`, true ready/warm-up flags, `device:"cpu"`, `dtype:"float32"`, `intra_threads:2`, `inter_threads:1`, `cuda_visible_devices:""`, `load_count:1`, `warmup_tokens`, startup timings |
| `encoded` | matching `request_id`, `role`, `input_sha256`, equal `text_sha256`, `profile`, `tokens`, `vector`, encode timings |
| `encode_error` | the same request/hash/profile binding, `code`, `tokens`; no vector |
| `failed` | a fixed `code`; no exception or input text |
| `stopped` | false ready/warm-up flags, `reason`, `worker_started`, `worker_exit_code`, `forced_group_stop`, `linux_cleanup_confirmed`, `group_reaped` |
| `cleanup_failed` | the same cleanup fields, with cleanup/reaped false |

Startup timings are `hash_audit_ms`, `model_load_ms`, `warmup_ms`, `total_ms`.
Encode timings are `tokenize_ms`, `inference_ms`, `total_ms`. Every timing is
finite and nonnegative. `input_too_long` carries `tokens > 512`; other legal
encoding failures use `encode_failed` and a count or null. Bad protocol ends the
worker instead of reflecting a malformed request's fields.

The manager validates every outgoing worker event, including exact profile,
current request identity, token bound, vector dimension and norm. It consumes
`worker_stopped` internally; that event is not a cleanup proof for the owner.

## Lifetime and cleanup

The manager watches owner stdin, worker stdout and bounded nonblocking writes in
one selector loop. Owner stop/EOF remains observable during model loading and
encoding. The startup gate is 10 seconds; manager preparation and encode
backstops are 90 and 60 seconds. The native actor may impose a shorter deadline
(currently five seconds for encoding), poison the owner, and request cleanup.

A worker is created with a new Linux session/process group. It also arms a
parent-death SIGTERM before exec, covering unexpected manager death without
waiting for Python's model loop to read stdin. Normal cleanup first sends stop
and closes stdin. It drains output while waiting up to 10 seconds, then signals
only that created group with SIGTERM (five seconds), then SIGKILL (five seconds)
if needed. It never finds processes by name, scans ports, or signals an unrelated
group. The group/session identity is checked when started and during cleanup.

`linux_cleanup_confirmed:true` / `group_reaped:true` require the created worker
to have been waited and no non-zombie live member to remain in its process group.
The fixed worker does not spawn child processes. An absent worker has null exit
code and both cleanup flags true. A cleanup failure is reported honestly rather
than promoted to success. **The Rust owner must still wait for the WSL bridge
process to exit**; a JSON receipt alone does not prove the Windows handle ended.

## Checks without models

```text
python -m py_compile scripts/local-memory/managed_embedding.py scripts/local-memory/embedding_worker.py
python -m unittest discover -s scripts/local-memory -p "test_*.py" -v
```

These tests use fake engines, fake process receipts and temporary synthetic
files. They do not start WSL, a service, a model, a GPU job, or a real child process.
Runtime latency, semantic quality and actual process-group cleanup still require
the separately authorized native-owner evaluation.
