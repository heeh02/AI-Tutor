"""Explicit real-model ASR preparation through one isolated main fixture.

Authoring or importing this file does not start a process. The explicit CLI
opt-in prepares synthetic silence only; no microphone, speaker, camera or PTZ
request exists in this diagnostic. Provider HOME/PATH are isolated.
"""
import argparse
import datetime
import http.client
import json
import os
from pathlib import Path
import shutil
import subprocess
import time

from database_compatibility_smoke import (MAX_RESPONSE_BYTES, OwnedWorkstation,
    SmokeFailure, check_idle, isolated_environment, require, sha256)

BUNDLE = ("managed_asr.py", "asr_service.py", "recognizer.py")
MODEL = "speech/sherpa-onnx-streaming-zipformer-bilingual-zh-en-2023-02-20"
WEIGHTS = ("tokens.txt", "encoder-epoch-99-avg-1.int8.onnx",
           "decoder-epoch-99-avg-1.onnx", "joiner-epoch-99-avg-1.int8.onnx")
PUBLIC = ("mode", "phase", "ready", "warmup_completed", "active", "busy",
          "retry_available", "shutting_down", "cleanup_confirmed")
OWNER = ("instance_id", "child_exited", "interpreter_exited", "forced",
         "cleanup_confirmed", "model_release_sent", "launcher_job_empty",
         "interpreter_job_empty", "interpreter_job_owned")
PROTECTED = ("runtime/speech-service.json", "runtime/local-asr-policy.json",
             "runtime/local-speech-policy.json", "runtime/tts-service.json")
ALLOWED = {("GET", "/api/state"), ("POST", "/api/audio/local-asr/retry"),
           ("POST", "/api/audio/local-asr/stop"), ("POST", "/api/shutdown")}


def write_new(path, value):
    with path.open("x", encoding="utf-8") as target:
        json.dump(value, target, ensure_ascii=False, indent=2)
        target.write("\n")


def protected_hashes(root):
    # Hash-only proof: no production credential value is parsed or logged.
    return {name: sha256(root / name) if (root / name).is_file() else None
            for name in PROTECTED}


def projection(local):
    result = {key: local[key] for key in PUBLIC if key in local}
    owner = local.get("ownership_stop")
    if isinstance(owner, dict):
        result["ownership_stop"] = {key: owner[key] for key in OWNER if key in owner}
    return result


def trace_recognizer_copy(path):
    """Instrument only this newly staged copy; never imports it or changes root source."""
    original_hash = sha256(path)
    source = path.read_text(encoding="utf-8")
    substitutions = (
        ("from pathlib import Path\n", "from pathlib import Path\nimport json as _lab_json\n"
         "import time as _lab_time\nimport faulthandler as _lab_fault\n\n"
         "def _lab_trace(stage):\n"
         "    target = Path(__file__).resolve().parents[2] / 'runtime/asr-load-trace.jsonl'\n"
         "    with target.open('a', encoding='utf-8') as trace:\n"
         "        trace.write(_lab_json.dumps({'stage': stage, 'perf_counter': _lab_time.perf_counter()}) + '\\n')\n"
         "        trace.flush()\n"),
        ("        import numpy as np\n        import sherpa_onnx\n",
         "        _lab_trace('numpy_import_before')\n"
         "        _lab_stack_path = Path(__file__).resolve().parents[2] / 'runtime/asr-load-stack.txt'\n"
         "        with _lab_stack_path.open('a', encoding='utf-8') as _lab_stack:\n"
         "            _lab_fault.dump_traceback_later(5, file=_lab_stack)\n"
         "            try:\n                import numpy as np\n"
         "            finally:\n                _lab_fault.cancel_dump_traceback_later()\n"
         "        _lab_trace('numpy_import_after')\n        _lab_trace('sherpa_import_before')\n"
         "        import sherpa_onnx\n        _lab_trace('sherpa_import_after')\n"),
        ("        self.model = sherpa_onnx.OnlineRecognizer.from_transducer(\n",
         "        _lab_trace('recognizer_create_before')\n"
         "        self.model = sherpa_onnx.OnlineRecognizer.from_transducer(\n"),
        ('            decoding_method="greedy_search", provider="cpu")\n',
         '            decoding_method="greedy_search", provider="cpu")\n'
         "        _lab_trace('recognizer_create_after')\n"),
    )
    for old, new in substitutions:
        require(source.count(old) == 1, "lab_trace_source_contract_changed")
        source = source.replace(old, new, 1)
    import ast
    ast.parse(source)
    path.write_text(source, encoding="utf-8")
    return {"enabled": True, "scope": "new fixture recognizer copy only",
            "original_recognizer_sha256": original_hash,
            "instrumented_recognizer_sha256": sha256(path),
            "trace_relative": "fixture/runtime/asr-load-trace.jsonl",
            "stack_relative": "fixture/runtime/asr-load-stack.txt",
            "stack_method": "single faulthandler traceback after 5s; no locals; cancelled after numpy import",
            "trace_fields": ["stage", "perf_counter"], "startup_deadline_changed": False}


def stage(root, output, ui, python, report, policies, trace_loading_lab=False):
    fixture = output / "fixture"
    shutil.copytree(ui, fixture / "frontend/dist")
    bundle = fixture / "bin/local-asr"
    bundle.mkdir(parents=True)
    report["bundle_sha256"] = {}
    for name in BUNDLE:
        source = root / "scripts/speech-service" / name
        shutil.copyfile(source, bundle / name)
        actual = sha256(bundle / name)
        require(actual == sha256(source), "staged_asr_program_hash_mismatch")
        report["bundle_sha256"][name] = actual
    report["trace_loading_lab"] = {"enabled": False}
    if trace_loading_lab:
        report["trace_loading_lab"] = trace_recognizer_copy(bundle / "recognizer.py")
        report["bundle_sha256"]["recognizer.py"] = sha256(bundle / "recognizer.py")
    report["fixed_model_files"] = []
    for name in WEIGHTS:
        source = root / "models" / MODEL / name
        require(source.is_file(), "fixed_asr_model_file_missing")
        target = fixture / "models" / MODEL / name
        target.parent.mkdir(parents=True, exist_ok=True)
        os.link(source, target)
        require(source.samefile(target), "model_file_not_hard_linked")
        report["fixed_model_files"].append({"relative": MODEL + "/" + name,
                                           "bytes": source.stat().st_size, "sha256": sha256(source)})
    require(python.is_absolute() and python.is_file(), "absolute_python_environment_missing")
    asr = {"protocol_version": 1, "mode": "managed", "python_executable": str(python)}
    tts = {"protocol_version": 1, "mode": "disabled", "warmup": "disabled"}
    runtime = fixture / "runtime"
    runtime.mkdir(exist_ok=True)
    for name, value in (("local-asr-policy.json", asr), ("local-speech-policy.json", tts)):
        write_new(runtime / name, value)
        policies[name] = value
    report["python_executable_sha256"] = sha256(python)
    # The report records policy semantics, not paths or private descriptors.
    report["policy"] = {"protocol_version": 1, "mode": "managed",
                        "python_executable": "explicit absolute installed environment"}
    return fixture


class AsrWorkstation(OwnedWorkstation):
    def raw(self, method, path, body=None, timeout=5):
        require((method, path) in ALLOWED, "diagnostic_endpoint_not_allowed")
        require(self.process.poll() is None and self.token is not None, "owned_process_not_available")
        connection = http.client.HTTPConnection("127.0.0.1", self.port, timeout=timeout)
        try:
            connection.request(method, path, json.dumps(body) if body is not None else None,
                {"Content-Type": "application/json", "Cookie": "ai_tutor_local=" + self.token})
            response = connection.getresponse()
            data = response.read(MAX_RESPONSE_BYTES + 1)
            require(len(data) <= MAX_RESPONSE_BYTES, "http_response_over_limit")
            return response.status, json.loads(data)
        finally:
            connection.close()

    def call(self, method, path, body=None, timeout=5):
        status, value = self.raw(method, path, body, timeout)
        require(status == 200, "unexpected_http_status_" + str(status))
        return value

    def stop(self):
        deadline = time.monotonic() + 35
        accepted, forced = False, False
        if self.process.poll() is None and self.token is not None:
            try:
                self.call("POST", "/api/shutdown", {}, timeout=10)
                accepted = True
            except (OSError, ValueError, http.client.HTTPException, SmokeFailure):
                pass
        try:
            self.process.wait(timeout=max(.01, deadline - time.monotonic()))
        except subprocess.TimeoutExpired:
            forced = True
            self.process.terminate()  # Only this diagnostic's exact Popen main.
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)
        self.stage.update(shutdown_http_accepted=accepted, forced_owned_main_stop=forced,
            main_exit_code=self.process.returncode,
            server_descriptor_removed_by_main=not self.descriptor.exists())
        if forced:
            self.stage["asr_cleanup_after_forced_main_stop"] = "unconfirmed"
        # Preserve natural removal evidence before deleting an owned stale secret.
        if self.descriptor.exists() and self.token is not None and self.process.poll() is not None:
            info = json.loads(self.descriptor.read_text(encoding="utf-8"))
            if (info.get("pid"), info.get("port"), info.get("token")) == (
                    self.process.pid, self.port, self.token):
                self.descriptor.unlink()
                self.stage["stale_owned_credential_removed_by_probe"] = True
        self.token = None


def confirm_stop(local, released=True):
    require(local.get("phase") == "stopped" and local.get("active") is False
            and local.get("busy") is False and local.get("retry_available") is True,
            "asr_stop_did_not_release_lifecycle")
    require(local.get("ready") is False and local.get("warmup_completed") is False,
            "stopped_service_still_claims_ready_or_warm")
    receipt = local.get("ownership_stop")
    require(isinstance(receipt, dict) and local.get("cleanup_confirmed") is True,
            "missing_owned_asr_cleanup_receipt")
    require(receipt.get("cleanup_confirmed") is True and receipt.get("child_exited") is True,
            "owned_child_exit_unconfirmed")
    if released:
        require(receipt.get("model_release_sent") is True and receipt.get("interpreter_exited") is True,
                "actual_model_interpreter_exit_unconfirmed")
        require(receipt.get("interpreter_job_owned") is True
                and receipt.get("interpreter_job_empty") is True
                and receipt.get("launcher_job_empty") is True, "owned_job_cleanup_unconfirmed")
    instance = receipt.get("instance_id")
    require(isinstance(instance, str) and len(instance) == 32, "invalid_cleanup_instance")
    return instance


def run(root, output, ui, executable, python, trace_loading_lab=False):
    require(os.name == "nt", "windows_main_fixture_required")
    output.mkdir(parents=True, exist_ok=False)
    report = {"test_type": "main_owned_local_asr_silent_preparation", "success": False,
        "synthetic_fixture_only": True, "devices_requested": False, "browser_opened": False,
        "microphone_requested": False, "speaker_requested": False, "camera_requested": False,
        "ptz_requested": False, "provider_requests_requested": False,
        "asr_user_audio_sent": False, "warmup_input": "service-owned synthetic silence",
        "legacy_service_contacted": False, "production_data_opened": False,
        "environment": "allowlisted OS plus fresh empty provider HOME and PATH",
        "cache_scope": "local model files are read and hashed before launch; not a cold-cache latency benchmark",
        "transitions": [], "main": {}, "errors": []}
    app, fixture, policies = None, output / "fixture", {}
    before = None
    started = time.perf_counter()
    def failure(error, stage_name):
        report["errors"].append({"stage": stage_name, "type": type(error).__name__,
            "code": str(error) if isinstance(error, SmokeFailure) else "unexpected_exception_no_body_logged"})

    try:
        require(executable.is_file(), "main_executable_missing")
        report["executable_sha256"] = sha256(executable)
        before = protected_hashes(root)
        report["protected_before_sha256"] = before
        fixture = stage(root, output, ui, python, report, policies, trace_loading_lab)
        write_new(output / "method.json", {key: report[key] for key in
            ("test_type", "executable_sha256", "bundle_sha256", "fixed_model_files", "python_executable_sha256", "policy", "cache_scope", "trace_loading_lab")})
        app = AsrWorkstation(executable, fixture, isolated_environment(fixture), report["main"])

        def state():
            value = app.call("GET", "/api/state")
            check_idle(value, report["main"])
            require(value.get("local_speech", {}).get("phase") == "disabled", "tts_not_disabled")
            require(value["audio"].get("tts_configured_engine") is None, "tts_client_unexpected")
            require(value.get("session") is None and value.get("learners") == [], "synthetic_learning_data_unexpected")
            local = value.get("local_asr")
            require(isinstance(local, dict), "local_asr_status_missing")
            item = projection(local)
            item["audio_asr_available"] = value["audio"].get("asr_ready")
            item["observed_ms"] = round((time.perf_counter() - started) * 1000, 3)
            marker = {k: v for k, v in item.items() if k != "observed_ms"}
            previous = report["transitions"][-1] if report["transitions"] else {}
            if marker != {k: v for k, v in previous.items() if k != "observed_ms"}:
                report["transitions"].append(item)
                write_new(output / f"state-{len(report['transitions']):03d}.json", item)
            return value, local

        app.ready()
        deadline = time.monotonic() + 95  # Observes the service's existing 60s load + 30s warm deadlines.
        while True:
            value, local = state()
            if local.get("active"):
                break
            require(local.get("phase") in ("starting", "loading", "warming"), "asr_failed_before_install")
            require(time.monotonic() < deadline, "asr_activation_observation_deadline")
            time.sleep(.025)
        require(local.get("phase") == "active" and local.get("ready") is True
                and local.get("warmup_completed") is True and value["audio"].get("asr_ready") is True,
                "loaded_warm_client_not_confirmed")
        report["first_active_ms"] = round((time.perf_counter() - started) * 1000, 3)
        report["active_receipt"] = projection(local)
        report["warming_phase_observed"] = any(row.get("phase") == "warming" for row in report["transitions"])
        require(app.raw("POST", "/api/audio/local-asr/retry", {})[0] == 400,
                "duplicate_retry_was_not_rejected")
        report["duplicate_retry_rejected"] = True
        stop_started = time.perf_counter()
        first = app.call("POST", "/api/audio/local-asr/stop", {}, timeout=15)
        report["first_stop_ms"] = round((time.perf_counter() - stop_started) * 1000, 3)
        report["first_stop_receipt"] = projection(first)
        first_instance = confirm_stop(first)
        value, local = state()
        require(value["audio"].get("asr_ready") is False, "future_capture_client_not_cleared")
        report["client_cleared_after_stop"] = True
        require(app.call("POST", "/api/audio/local-asr/retry", {}).get("queued") is True,
                "explicit_retry_not_queued")
        deadline = time.monotonic() + 10
        while True:
            value, local = state()
            require(not local.get("active"), "early_cancel_window_missed_client_already_installed")
            require(local.get("phase") in ("starting", "loading", "warming"), "retry_failed_before_cancel")
            if local.get("phase") in ("loading", "warming"):
                break  # Private bootstrap was released; interpreter identity exists.
            require(time.monotonic() < deadline, "retry_loading_not_observed")
            time.sleep(.005)
        report["early_cancel_phase"] = local["phase"]
        second = app.call("POST", "/api/audio/local-asr/stop", {}, timeout=15)
        report["retry_stop_receipt"] = projection(second)
        require(confirm_stop(second) != first_instance, "retry_reused_old_owned_instance")
        report["new_instance_and_early_cancel_confirmed"] = True
        value, local = state()
        require(value["audio"].get("asr_ready") is False, "cancelled_retry_installed_client")
        require(not (fixture / "runtime/speech-service.json").exists(), "managed_service_wrote_public_descriptor")
        require(not (fixture / "runtime/tts-service.json").exists(), "unexpected_tts_descriptor")
        report["checks_passed"] = True
    except (Exception, KeyboardInterrupt) as error:
        failure(error, "lifecycle")
    finally:
        if app is not None:
            if report["errors"] and app.process.poll() is None and app.token is not None:
                try:
                    # Recover proof from the same fixture main and credential;
                    # this never contacts a public ASR descriptor or legacy port.
                    code, stopped = app.raw("POST", "/api/audio/local-asr/stop", {}, timeout=15)
                    if code != 200:
                        stopped = app.call("GET", "/api/state").get("local_asr", {})
                    report["failure_asr_stop"] = {"http_status": code, "terminal": projection(stopped)}
                    write_new(output / "failure-asr-stop.json", report["failure_asr_stop"])
                    require(code == 200, "failure_asr_stop_not_confirmed")
                    owner = stopped.get("ownership_stop")
                    released = isinstance(owner, dict) and owner.get("model_release_sent") is True
                    confirm_stop(stopped, released=released)
                    report["failure_asr_stop"]["cleanup_proof_validated"] = True
                except (Exception, KeyboardInterrupt) as error:
                    failure(error, "asr_cleanup_before_main_shutdown")
            try:
                app.stop()
            except (Exception, KeyboardInterrupt) as error:
                failure(error, "main_cleanup")
        try:
            if fixture is not None and (app is None or app.process.poll() is not None):
                for name, expected in policies.items():
                    path = fixture / "runtime" / name
                    require(path.is_file() and json.loads(path.read_text(encoding="utf-8")) == expected,
                            "owned_fixture_policy_changed")
                    path.unlink()  # Exact files created above; retain all fixture directories/model links.
                report["owned_fixture_policies_removed"] = True
            after = protected_hashes(root)
            report["protected_after_sha256"] = after
            require(before is not None and before == after, "production_descriptor_or_policy_changed")
            require(sha256(executable) == report.get("executable_sha256"), "input_executable_changed")
            report["production_descriptors_and_policies_unchanged"] = True
        except (Exception, KeyboardInterrupt) as error:
            failure(error, "fixture_cleanup_or_protected_state")
        report["success"] = bool(report.get("checks_passed") and not report["errors"]
            and report["main"].get("shutdown_http_accepted")
            and report["main"].get("main_exit_code") == 0
            and report["main"].get("server_descriptor_removed_by_main")
            and not report["main"].get("forced_owned_main_stop")
            and report.get("owned_fixture_policies_removed"))
        report["elapsed_ms"] = round((time.perf_counter() - started) * 1000, 3)
        report["checked_at"] = datetime.datetime.now(datetime.timezone.utc).isoformat()
        if not report["success"]:
            write_new(output / "failure.json", {"errors": report["errors"], "main": report["main"],
                "asr_stop": report.get("failure_asr_stop"),
                "last_state": report["transitions"][-1] if report["transitions"] else None})
        write_new(output / "report.json", report)
        print(json.dumps({"success": report["success"], "report": str(output / "report.json")}))
    return 0 if report["success"] else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--real-local-synthetic", action="store_true", required=True)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--executable", type=Path, required=True)
    parser.add_argument("--python-executable", type=Path, required=True)
    parser.add_argument("--ui", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--trace-loading-lab", action="store_true",
                        help="Instrument only the new fixture recognizer copy; preserves production source and 60s load budget")
    args = parser.parse_args()
    return run(args.root.resolve(), args.output.resolve(), args.ui.resolve(),
               args.executable.resolve(), args.python_executable.resolve(), args.trace_loading_lab)


if __name__ == "__main__":
    raise SystemExit(main())
