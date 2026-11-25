"""Own one Qwen service, run exactly one zh/en synthetic model chain, then stop.

This invokes an already-built ignored Rust test binary, never Cargo. The default
is transport-only. --production-playback explicitly uses the native output owner
and default output device; callback receipts are not measured acoustic output.
"""
import argparse
import hashlib
import http.client
import json
import os
from pathlib import Path
import queue
import secrets
import shutil
import subprocess
import sys
import threading
import time
import uuid

LAB = Path(__file__).resolve().parents[1] / "speech-lab"
sys.path.insert(0, str(LAB))
from probe_managed_tts import (ProbeFailure, require_running, stderr_reader,
                               stdout_reader, stop_wrapper, wait_ready)

TEST = "audio::streaming_asr::voice_model_chain_diagnostic::real_synthetic_asr_teaching_tts_transport_chain"
PLAYBACK_TEST = "server::segment_playback_diagnostic::real_synthetic_production_segment_playback"
GRAPH_SHA = "a79c0b71f391363df7a3dde0c4d5d6386315dfde997f7940449c96ddb451b9ea"
MODEL_REVISION = "85e237c12c027371202489a0ec509ded67b5e4b5"
BUNDLE = ("managed_tts.py", "tts_gpu_lease.sh", "cuda_link_env.sh",
          "start_qwen_omni.sh", "start_cosy_omni.sh", "qwen3_tts_12gb.yaml",
          "cosyvoice3_12gb_candidate.yaml")
WARM = (("zh", "先让小车停下来。", 9900), ("en", "First, stop the car.", 9901))
PROTECTED = ("bin/ai-tutor.exe", "runtime/server.json", "runtime/speech-service.json",
             "runtime/tts-service.json", "runtime/local-asr-policy.json",
             "runtime/local-speech-policy.json", "runtime/tutor-delivery.json",
             "prompts/tutor_core.md")
CHAIN_SECONDS = 420  # Outer diagnostic limit; no production timeout is changed.


def digest(path):
    value = hashlib.sha256()
    with path.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            value.update(chunk)
    return value.hexdigest()


def save(path, value):
    # This entire output directory is created exclusively by this invocation.
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def protected(root):
    return {name: digest(root / name) if (root / name).is_file() else None for name in PROTECTED}


def stage(root, output, executable, playback=False):
    previous_root = root / "runtime/goal-baselines/20261104/main-managed-qwen-first"
    previous = json.loads((previous_root / "report.json").read_text(encoding="utf-8"))
    if previous.get("success") is not True or previous.get("main_exit_code") != 0:
        raise ProbeFailure("graph_fixture_previous_success_missing")
    original = previous_root / "fixture"
    if (original / "runtime/local-speech-policy.json").exists():
        raise ProbeFailure("graph_fixture_policy_not_cleaned")
    for name, expected in previous["bundle_sha256"].items():
        if digest(original / "bin/local-speech" / name) != expected:
            raise ProbeFailure("previous_graph_bundle_changed")
    if digest(original / "bin/local-speech/qwen3_tts_12gb.yaml") != GRAPH_SHA:
        raise ProbeFailure("previous_graph_configuration_changed")

    fixture = output / "tts-fixture"
    bundle = fixture / "bin/local-speech"
    bundle.mkdir(parents=True)
    hashes = {}
    for name in BUNDLE:
        source, target = root / "scripts/speech-lab" / name, bundle / name
        shutil.copyfile(source, target)
        hashes[name] = digest(source)
        if digest(target) != hashes[name]:
            raise ProbeFailure("staged_bundle_changed")
    if hashes["qwen3_tts_12gb.yaml"] != GRAPH_SHA:
        raise ProbeFailure("current_graph_configuration_changed")
    links = previous["model_files_hard_linked"]
    for relative in links:
        source = original / "models" / relative
        if not source.samefile(root / "models" / relative):
            raise ProbeFailure("graph_model_hardlink_identity_changed")
        target = fixture / "models" / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        os.link(source, target)
    model = fixture / "models/speech/Qwen3-TTS-12Hz-0.6B-CustomVoice"
    manifest = json.loads((model / "download-manifest.json").read_text(encoding="utf-8"))
    if manifest.get("revision") != MODEL_REVISION:
        raise ProbeFailure("graph_model_revision_changed")
    for row in manifest["files"]:
        if digest(model / row["file"]) != row["sha256"]:
            raise ProbeFailure("graph_model_hash_changed")
    relative = "runtime/speech-gpu/omni-env-path.txt"
    (fixture / relative).parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(original / relative, fixture / relative)
    method = {"version": 2 if playback else 1, "test": PLAYBACK_TEST if playback else TEST,
              "languages": ["zh", "en"],
              "teaching_calls_per_language": 2 if playback else 1, "automatic_retry": False,
              "test_executable_sha256": digest(executable), "runner_sha256": digest(Path(__file__)),
              "teaching_policy_sha256": digest(root / "prompts/tutor_core.md"),
              "teaching_schema_sha256": digest(root / "implementation-package/AI_Tutor_Codex_Implementation_Package/schemas/tutor_reply.schema.json"),
              "context_source_sha256": digest(root / "rust/ai-tutor/src/context.rs"),
              "teaching_adapter_source_sha256": digest(root / "rust/ai-tutor/src/kimi_acp_stream.rs"),
              "ownership_helper_sha256": digest(LAB / "probe_managed_tts.py"),
              "bundle_sha256": hashes, "graph_configuration_sha256": GRAPH_SHA,
              "model_revision": MODEL_REVISION, "model_manifest_sha256": digest(model / "download-manifest.json"),
              "model_files_verified": len(manifest["files"]), "model_files_hard_linked": len(links),
              "environment_pointer_sha256": digest(fixture / relative),
              "previous_fixture_report_sha256": digest(previous_root / "report.json"),
              "cache_scope": "Fresh fixture cache; original fixture and models are not rewritten",
              "warmup": [{"language": language, "text": text, "seed": seed} for language, text, seed in WARM],
              "readiness_budget_seconds": 240, "warmup_budget_per_language_seconds": 120,
              "chain_budget_per_language_seconds": CHAIN_SECONDS,
              "tts_stop_budget_seconds": 20,
              "microphone_opened": False, "speaker_output": playback,
              "token_transport": "Private wrapper stdin bootstrap; fresh credential in diagnostic process environment and inherited by its children unless explicitly cleared; runner logs redact this credential",
              "raw_pcm_saved": False, "production_descriptor_or_policy_written": False,
              "scope": "Real ASR, teaching provider and TTS transports; not Workstation voice-loop/device/acoustic acceptance"}
    if playback:
        method.pop("speaker_output")
        method.update(native_output_requested=True, speaker_output_measured=False,
                      simulated_display_ack=True,
                      cases=["stop_then_late_ack", "restart_complete"],
                      scope="Real Workstation dispatch, segment scheduler and native AudioService/CPAL; fixed WAV input and simulated display ACK; outer runner owns TTS, diagnostic owns ASR; not microphone/VAD/rearm/browser/acoustic acceptance")
    save(output / "method.json", method)
    return fixture


def wsl_path(path):
    result = subprocess.run(["wsl.exe", "-d", "Ubuntu", "--", "wslpath", "-u", str(path)],
                            stdin=subprocess.DEVNULL, capture_output=True, timeout=15, check=True,
                            creationflags=subprocess.CREATE_NO_WINDOW)
    value = result.stdout.decode("utf-8").strip()
    if not value.startswith("/") or "\n" in value:
        raise ProbeFailure("fixture_path_conversion_failed")
    return value.rstrip("/")


def warm(language, text, seed, token, process, events, report, started):
    row = {"language": language, "seed": seed, "success": False, "raw_pcm_saved": False}
    timer = time.perf_counter()
    deadline = time.monotonic() + 120
    connection = http.client.HTTPConnection("127.0.0.1", 18868, timeout=5)
    count, nonzero, first, sha = 0, False, None, hashlib.sha256()
    try:
        body = {"model": "tutor-qwen-tts", "input": text, "stream": True,
                "stream_format": "audio", "response_format": "pcm", "seed": seed,
                "voice": "vivian" if language == "zh" else "ryan",
                "language": "Chinese" if language == "zh" else "English"}
        connection.request("POST", "/v1/audio/speech", json.dumps(body),
                           {"Content-Type": "application/json", "Authorization": "Bearer " + token})
        transport = connection.sock
        remaining = deadline - time.monotonic()
        if transport is None or remaining <= 0:
            raise ProbeFailure("warmup_connection_or_deadline_failed")
        transport.settimeout(remaining)
        response = connection.getresponse()
        if response.status != 200:
            raise ProbeFailure("warmup_http_rejected")
        if response.getheader("Content-Type", "").split(";", 1)[0].strip().lower() not in {"audio/pcm", "application/octet-stream"}:
            raise ProbeFailure("warmup_pcm_contract_rejected")
        while True:
            require_running(process, events, report, started)
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise ProbeFailure("warmup_deadline_exceeded")
            # Keep this exact transport even for Connection:close, when
            # HTTPConnection relinquishes its reference to HTTPResponse.
            if transport.fileno() != -1:
                transport.settimeout(min(15, remaining))
            packet = response.read1(8192)
            if not packet:
                break
            first = first if first is not None else (time.perf_counter() - timer) * 1000
            count += len(packet)
            if count > 24000 * 2 * 180:
                raise ProbeFailure("warmup_pcm_budget_exceeded")
            nonzero |= any(packet)
            sha.update(packet)
        if time.monotonic() > deadline or not count or count % 2 or not nonzero:
            raise ProbeFailure("warmup_pcm_incomplete")
        row["success"] = True
    finally:
        connection.close()
        row.update(elapsed_ms=(time.perf_counter()-timer)*1000,
                   first_pcm_received_ms=first, bytes_received=count, pcm_sha256=sha.hexdigest())
        report["warmup"].append(row)
    return row


def check_chain(value, language):
    if (value.get("passed") is not True or value.get("language") != language
            or value.get("tts_engine") != "qwen" or value.get("stage") != "completed"
            or value.get("teaching_request_limit") != 1
            or value.get("provider_cleanup_confirmed") is not True
            or value.get("protected_files_unchanged") is not True
            or any(value.get(key) is not False for key in
                   ("microphone_opened", "speaker_output", "ambient_audio_used", "raw_pcm_saved",
                    "input_closed_claimed", "playback_receipts_created", "voice_loop_permission_or_rearm_tested"))):
        raise ProbeFailure("chain_report_contract_failed")
    owned = value.get("owned_asr_stop", {})
    if any(owned.get(key) is not True for key in
           ("cleanup_confirmed", "child_exited", "interpreter_exited", "launcher_job_empty",
            "interpreter_job_empty", "interpreter_job_owned")):
        raise ProbeFailure("chain_asr_owned_cleanup_unconfirmed")
    if (value.get("asr", {}).get("completed") is not True
            or value.get("asr", {}).get("whisper_fallback_used") is not False
            or value.get("asr", {}).get("fallback_requested") is not False
            or value.get("teaching", {}).get("completed") is not True
            or value.get("provider_metrics", {}).get("requests") != 1
            or value.get("provider_metrics", {}).get("acp_segment_requests") != 1):
        raise ProbeFailure("chain_model_stage_incomplete")
    segments = value.get("tts", [])
    if (not segments or len(segments) != value.get("teaching", {}).get("segment_count")
            or any(row.get("completed") is not True or row.get("samples_received", 0) <= 0
                   or row.get("speaker_output") is not False for row in segments)):
        raise ProbeFailure("chain_tts_segment_transport_incomplete")


def check_playback(value, language):
    if (value.get("test_type") != "real_synthetic_production_segment_playback"
            or value.get("language") != language or value.get("passed") is not True
            or value.get("production_audio_owner") is not True
            or value.get("simulated_display_ack") is not True
            or value.get("speaker_output_measured") is not False
            or value.get("protected_files_unchanged") is not True):
        raise ProbeFailure("playback_report_contract_failed")
    if any(value.get(key) is not False for key in
           ("microphone_opened", "camera_opened", "ptz_enabled", "raw_pcm_saved")):
        raise ProbeFailure("playback_scope_contract_failed")
    owned = value.get("owned_asr_stop", {})
    if any(owned.get(key) is not True for key in
           ("cleanup_confirmed", "child_exited", "interpreter_exited", "launcher_job_empty",
            "interpreter_job_empty", "interpreter_job_owned")):
        raise ProbeFailure("playback_asr_cleanup_unconfirmed")
    if (value.get("asr", {}).get("completed") is not True
            or value.get("asr", {}).get("fallback_requested") is not False
            or value.get("provider_metrics", {}).get("requests") != 2
            or value.get("provider_metrics", {}).get("acp_segment_requests") != 2):
        raise ProbeFailure("playback_model_stage_incomplete")
    rounds = value.get("rounds", [])
    if len(rounds) != 2 or [row.get("case") for row in rounds] != ["stop_then_late_ack", "restart_complete"]:
        raise ProbeFailure("playback_round_contract_failed")
    # Detailed owner/gate assertions are made by the Rust diagnostic; overlap
    # remains an observed result, never a prerequisite manufactured by waiting.
    if any(row.get("passed") is not True for row in rounds):
        raise ProbeFailure("playback_round_incomplete")


def run_chain(root, output, executable, language, token, process, events, report, started, playback=False):
    run = ("voice-playback-" if playback else "voice-chain-") + uuid.uuid4().hex + "-" + language
    child_env = os.environ.copy()
    child_env.update(AI_TUTOR_REAL_VOICE_CHAIN_SYNTHETIC="1", AI_TUTOR_VOICE_CHAIN_LANGUAGE=language,
                     AI_TUTOR_VOICE_CHAIN_TTS_ENGINE="qwen", AI_TUTOR_VOICE_CHAIN_RUN=run,
                     AI_TUTOR_VOICE_CHAIN_TTS_TOKEN=token)
    if playback:
        child_env["AI_TUTOR_REAL_SEGMENT_PLAYBACK"] = "1"
    child = subprocess.Popen([str(executable), PLAYBACK_TEST if playback else TEST, "--exact", "--ignored", "--nocapture", "--test-threads=1"],
                             cwd=root, env=child_env, stdin=subprocess.DEVNULL,
                             stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                             creationflags=subprocess.CREATE_NO_WINDOW)
    child_env.pop("AI_TUTOR_VOICE_CHAIN_TTS_TOKEN")
    interrupted = False
    try:
        stdout, stderr = child.communicate(timeout=CHAIN_SECONDS)
    except (subprocess.TimeoutExpired, KeyboardInterrupt):
        interrupted = True
        child.terminate()  # Exact diagnostic process only; never a discovered PID.
        try:
            stdout, stderr = child.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            child.kill()
            stdout, stderr = child.communicate(timeout=5)
    for name, data in ((language + ".stdout.log", stdout), (language + ".stderr.log", stderr)):
        (output / name).write_bytes(data.replace(token.encode("ascii"), b"[REDACTED]"))
    path = root / "runtime/goal-baselines/20261104" / run / "report.json"
    report["chains"].append({"language": language, "exit_code": child.returncode,
                             "interrupted": interrupted, "report": str(path), "success": False})
    if interrupted or child.returncode != 0:
        raise ProbeFailure("chain_process_failed_or_interrupted_no_retry")
    value = json.loads(path.read_text(encoding="utf-8"))
    (check_playback if playback else check_chain)(value, language)
    require_running(process, events, report, started)
    report["chains"][-1].update(success=True, report_sha256=digest(path))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--real-local-synthetic", action="store_true", required=True)
    parser.add_argument("--production-playback", action="store_true",
                        help="Explicitly play synthetic teaching replies through the native default output; two requests per language")
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--test-executable", type=Path, required=True, help="Prebuilt ai_tutor library test .exe; never Cargo")
    parser.add_argument("--output", type=Path, required=True, help="Fresh nonexistent evidence directory")
    args = parser.parse_args()
    if os.name != "nt":
        parser.error("This runner owns a Windows wsl.exe bridge")
    root, output, executable = args.root.resolve(), args.output.resolve(), args.test_executable.resolve()
    if not executable.is_file():
        parser.error("Build and explicitly select the library test executable first")
    output.mkdir(parents=True, exist_ok=False)
    instance, token = uuid.uuid4().hex, secrets.token_urlsafe(32)
    report = {"test_type": "owned_qwen_real_synthetic_production_playback" if args.production_playback else "owned_qwen_real_synthetic_model_chain", "success": False,
              "instance_id": instance, "starting": False, "ready": False, "warm": False,
              "stopped": False, "linux_cleanup_confirmed": False, "bridge_terminated_only": False,
              "wrapper_events": [], "authentication": {}, "errors": [], "warmup": [], "chains": [],
              "microphone_opened": False, "speaker_output": args.production_playback, "raw_pcm_saved": False,
              "automatic_retry": False, "production_descriptor_or_policy_written": False}
    if args.production_playback:
        report.pop("speaker_output")
        report.update(native_output_requested=True, speaker_output_measured=False,
                      simulated_display_ack=True)
    process, readers, events, before = None, [], queue.Queue(), None
    started = time.monotonic()
    try:
        before = protected(root)
        report["protected_before_sha256"] = before
        fixture = stage(root, output, executable, args.production_playback)
        report["method_sha256"] = digest(output / "method.json")
        linux_root = wsl_path(fixture)
        process = subprocess.Popen(["wsl.exe", "-d", "Ubuntu", "--", "python3",
                                    linux_root + "/bin/local-speech/managed_tts.py", "--root", linux_root,
                                    "--engine", "qwen", "--instance", instance],
                                   stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                   creationflags=subprocess.CREATE_NO_WINDOW)
        for target, params in (
                (stdout_reader, (process.stdout, events, instance, "qwen")),
                (stderr_reader, (process.stderr, output / "wrapper.stderr.log", token, events))):
            reader = threading.Thread(target=target, args=params, daemon=True)
            reader.start()
            readers.append(reader)
        process.stdin.write((json.dumps({"protocol_version": 1, "bearer_token": token}) + "\n").encode())
        process.stdin.flush()
        wait_ready(process, events, report, started, "qwen", token)
        for language, text, seed in WARM:
            warm(language, text, seed, token, process, events, report, started)
            save(output / "report.json", report)
        report["warm"] = True
        for language in ("zh", "en"):
            require_running(process, events, report, started)
            run_chain(root, output, executable, language, token, process, events, report, started, args.production_playback)
            save(output / "report.json", report)
    except (Exception, KeyboardInterrupt) as error:
        report["errors"].append(str(error) if isinstance(error, ProbeFailure) else type(error).__name__)
    finally:
        if process is not None:
            try:
                stop_wrapper(process, events, readers, instance, report, started)
            except (Exception, KeyboardInterrupt) as error:
                report["errors"].append("owned_tts_cleanup_" + type(error).__name__)
                report["linux_cleanup_confirmed"] = False
        try:
            after = protected(root)
            report["protected_after_sha256"] = after
            report["protected_files_unchanged"] = before is not None and before == after
        except OSError:
            report["protected_files_unchanged"] = False
            report["errors"].append("protected_hash_check_failed")
        report["total_ms"] = (time.monotonic() - started) * 1000
        report["success"] = (not report["errors"] and report["ready"] and report["warm"]
                             and report["stopped"] and report["linux_cleanup_confirmed"]
                             and report["protected_files_unchanged"]
                             and len(report["chains"]) == 2 and all(row["success"] for row in report["chains"]))
        save(output / "report.json", report)
    print(json.dumps({"success": report["success"], "report": str(output / "report.json"),
                      "chains_completed": sum(row["success"] for row in report["chains"]),
                      "linux_cleanup_confirmed": report["linux_cleanup_confirmed"]}))
    return 0 if report["success"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
