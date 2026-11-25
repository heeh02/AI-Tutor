"""Explicit camera + managed speech coexistence and periodic warmup; never a teaching-loop test.

No import-time execution. Only the root operator runs the explicit CLI opt-in.
No microphone, speaker, person selection, PTZ, private service token or model API.
"""
import argparse
import datetime
import hashlib
import http.client
import importlib.metadata
import json
import os
from pathlib import Path
import time

from camera_stream_smoke import observe_workers, workers_alive, number
from coexistence_assets import stage, protected_hashes
from coexistence_preview import Preview, WIRE_METHOD
from database_compatibility_smoke import MAX_RESPONSE_BYTES, SmokeFailure, isolated_environment, require, sha256, write_json
from local_asr_lifecycle_smoke import AsrWorkstation, confirm_stop, projection as asr_projection

METHOD = Path(__file__).with_name("camera_speech_coexistence_method_v1.json")
ALLOWED = {("GET", "/api/state"), ("GET", "/api/devices/cameras"),
           ("POST", "/api/camera/open"), ("POST", "/api/camera/close"), ("POST", "/api/shutdown"),
           *(('POST', '/api/audio/' + service + '/' + action)
             for service in ('local-asr', 'local-speech') for action in ('stop', 'retry'))}
PUBLIC = ("mode", "phase", "ready", "warmup_completed", "active", "busy", "retry_available",
          "shutting_down", "cleanup_confirmed", "linux_cleanup_confirmed", "client_deactivation_confirmed")


class Workstation(AsrWorkstation):
    # Reuses OwnedWorkstation startup/identity checks and the ASR diagnostic's
    # bounded 35s shutdown, with a domain-specific endpoint allowlist.
    def raw(self, method, path, body=None, timeout=5):
        require((method, path) in ALLOWED, "coexistence_endpoint_not_allowed")
        require(self.process.poll() is None and self.token is not None, "owned_main_unavailable")
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


def check_state(state):
    require(state["audio"].get("recording") is False and state["audio"].get("state") == "idle",
            "unexpected_capture_or_playback")
    ptz = state["ptz"]
    require(ptz.get("mode") == "off" and ptz.get("commands_sent") == 0
            and ptz.get("command_attempts") == 0 and ptz.get("hardware_stop_command_sent") is False,
            "unexpected_ptz_activity")
    require(state["camera"].get("selected_track_key") is None, "unexpected_target_selection")
    kimi = state["kimi"]
    require(kimi.get("available") is False and kimi.get("running", 0) == 0
            and kimi["metrics"].get("requests", 0) == 0, "unexpected_remote_provider")
    require(state.get("session") is None and state.get("learners") == [], "unexpected_learning_data")


def warm_receipt(state, engine):
    for field in ("local_asr", "local_speech"):
        local = state[field]
        require(local.get("phase") == "active" and all(local.get(k) is True for k in ("ready", "warmup_completed", "active")),
                "speech_services_not_both_active")
    require(state["audio"].get("asr_ready") is True and state["audio"].get("tts_configured_engine") == engine + "_pcm",
            "prepared_clients_not_installed")
    receipt = state["local_speech"].get("receipt", {})
    rows = receipt.get("languages", [])
    require(len(rows) == 2 and {row.get("language") for row in rows} == {"zh", "en"}, "bilingual_warm_receipt_missing")
    languages = []
    for row in rows:
        samples = row.get("samples")
        require(type(samples) is int and samples > 0 and row.get("transport", {}).get("samples") == samples,
                "warm_pcm_not_validated")
        languages.append({"language": row["language"], "samples": samples,
                          "transport_numeric": {k: v for k, v in row["transport"].items() if number(v) or isinstance(v, bool)}})
    return {"asr": {"source": "managed synthetic one-second silence warmup", "ready": True,
                    "warmup_completed": True, "spoken_wav_transcription_tested": False},
            "tts": {"languages": languages, "preparation_ms": receipt.get("preparation_ms"),
                    "speaker_output_measured": False}}


def cycle_offsets(duration, period):
    return list(range(period, duration, period))


def confirm_tts_stop(result):
    require(result.get("phase") == "stopped" and result.get("active") is False
            and result.get("busy") is False and result.get("linux_cleanup_confirmed") is True
            and result.get("client_deactivation_confirmed") is True, "owned_tts_cleanup_unconfirmed")
    owner = result.get("ownership_stop", {})
    receipt = owner.get("receipt", {})
    instance = receipt.get("instance_id")
    require(owner.get("linux_cleanup_confirmed") is True and owner.get("bridge_exit_code") == 0
            and owner.get("normal_owner_stop") is True and receipt.get("state") == "stopped"
            and isinstance(instance, str) and len(instance) == 32 and result.get("instance_id") == instance,
            "owned_tts_stop_identity_unconfirmed")
    return instance


def run(args):
    root, output, ui, executable, python = (p.resolve() for p in
        (args.root, args.output, args.ui, args.exe, args.python_executable))
    require(os.name == "nt" and args.real_camera_local_synthetic, "explicit_windows_camera_synthetic_required")
    require(not output.exists() and not output.is_relative_to(ui), "fresh_output_required")
    require(executable.is_file() and (ui / "index.html").is_file(), "explicit_release_or_ui_missing")
    output.mkdir(parents=True)
    method = json.loads(METHOD.read_text(encoding="utf-8"))
    write_json(output / "method.json", method)
    report = {"method_id": method["method_id"], "method_sha256": sha256(METHOD),
              "wire_method_sha256": sha256(WIRE_METHOD), "script_sha256": sha256(Path(__file__)),
              "executable_sha256": sha256(executable), "requested_seconds": args.duration_seconds,
              "run_kind": "full" if args.duration_seconds == 1800 else "short",
              "passed": False, "complete_teaching_pipeline_tested": False,
              "microphone_requested": False, "speaker_requested": False, "raw_media_saved": False,
              "ptz_api_calls": 0, "target_selection_api_calls": 0, "remote_provider_calls": 0,
              "production_database_opened": False, "private_speech_tokens_read": False,
              "main": {}, "cycles": [], "cleanup": {}, "errors": []}
    report["helper_sha256"] = {p: sha256(Path(__file__).with_name(p)) for p in
        ("coexistence_assets.py", "coexistence_preview.py", "camera_stream_smoke.py",
         "local_asr_lifecycle_smoke.py", "local_voice_lifecycle_smoke.py", "database_compatibility_smoke.py")}
    app = preview = None
    fixture = output / "fixture"
    sources, policies, observed, stopped_instances, stopped_tts_instances = {}, {}, {}, set(), set()
    protected = None
    started = time.perf_counter()
    samples_file = None
    previous_marker = None
    def failure(stage_name, error):
        report["errors"].append({"stage": stage_name,
            "code": str(error) if isinstance(error, SmokeFailure) else type(error).__name__})
    try:
        protected = protected_hashes(root)
        report["protected_before_sha256"] = protected
        versions = {name: importlib.metadata.version(name) for name in ("websockets", "Pillow", "psutil")}
        require(versions == {"websockets": "15.0.1", "Pillow": "12.3.0", "psutil": "7.0.0"}, "diagnostic_dependency_version_changed")
        from websockets.sync.client import connect
        from PIL import Image
        import psutil
        report["dependencies"] = versions
        fixture, policies, sources, staged = stage(root, output, ui, python, args.engine, args.distribution,
            args.references_manifest.resolve() if args.references_manifest else None)
        report["staged"] = staged
        app = Workstation(executable, fixture, isolated_environment(fixture), report["main"])
        check_state(app.ready())
        devices = app.call("GET", "/api/devices/cameras")
        require(isinstance(devices, list), "camera_catalog_not_array")
        report["enumerated_camera_count"] = len(devices)
        if args.device_id_file:
            device = args.device_id_file.read_text(encoding="utf-8-sig").strip()
            require(sum(row.get("device_id") == device for row in devices) == 1, "selected_camera_not_unique")
        else:
            require(args.single_camera and len(devices) == 1, "explicit_single_camera_not_unique")
            device = devices[0].get("device_id")
        require(isinstance(device, str) and device, "camera_id_invalid")
        report["selected_device_id_sha256"] = hashlib.sha256(device.encode()).hexdigest()
        app.call("POST", "/api/camera/open", {"device_id": device})
        preview = Preview(app, output, connect, Image, method)
        preview.start()
        samples_file = (output / "state-resources.jsonl").open("x", encoding="utf-8")

        def observe():
            nonlocal previous_marker
            require(preview.error is None and preview.thread.is_alive(), "preview_failed_" + str(preview.error))
            value = app.call("GET", "/api/state")
            check_state(value)
            observe_workers(app, executable, observed, psutil)
            row = {"elapsed_ms": (time.perf_counter() - started) * 1000,
                   "camera_phase": value["camera"].get("state"),
                   **{field: {key: value[field][key] for key in PUBLIC if key in value[field]}
                      for field in ("local_asr", "local_speech")}}
            processes = []
            owner = psutil.Process(app.process.pid)
            for process in [owner, *owner.children(recursive=True)]:
                try:
                    cpu = process.cpu_times()
                    processes.append({"pid": process.pid, "created": process.create_time(),
                        "rss_bytes": process.memory_info().rss, "cpu_user_system_seconds": cpu.user + cpu.system})
                except psutil.NoSuchProcess:
                    continue
            row["windows_owned_processes"] = processes
            samples_file.write(json.dumps(row, separators=(",", ":")) + "\n")
            samples_file.flush()
            marker = (row["local_asr"], row["local_speech"])
            if marker != previous_marker:
                report["last_state"] = row
                previous_marker = marker
            return value

        deadline = time.perf_counter() + method["camera_first_frame_timeout_seconds"]
        while not preview.ready.is_set():
            observe()
            require(time.perf_counter() < deadline, "camera_not_ready")
            time.sleep(.1)
        for name, value in policies.items():
            path = fixture / "runtime" / name
            require(json.loads(path.read_text(encoding="utf-8"))["mode"] == "disabled", "fixture_policy_changed_before_enable")
            path.write_text(json.dumps(value, ensure_ascii=False, indent=2), encoding="utf-8")

        def start_services():
            for service in ("local-asr", "local-speech"):
                require(app.call("POST", f"/api/audio/{service}/retry", {}).get("queued") is True,
                        "explicit_service_retry_not_queued")

        def wait_active(until):
            while True:
                value = observe()
                for field in ("local_asr", "local_speech"):
                    require(value[field].get("phase") not in ("degraded", "configuration_error", "cleanup_failed", "stopped", "cancelled"),
                            "speech_service_failed_during_preparation")
                if all(value[field].get("active") is True for field in ("local_asr", "local_speech")):
                    return warm_receipt(value, args.engine)
                require(time.perf_counter() < until, "joint_activation_deadline")
                time.sleep(method["state_poll_seconds"])

        def stop_services(label):
            receipts = {}
            # Always attempt both owned cleanups even if one fails.
            errors = []
            for service, timeout in (("local-asr", method["asr_stop_http_timeout_seconds"]),
                                     ("local-speech", method["tts_stop_http_timeout_seconds"])):
                try:
                    result = app.call("POST", f"/api/audio/{service}/stop", {}, timeout=timeout)
                    if service == "local-asr":
                        receipts[service] = asr_projection(result)
                        instance = confirm_stop(result)
                        require(instance not in stopped_instances, "asr_cycle_reused_owned_instance")
                        stopped_instances.add(instance)
                    else:
                        receipts[service] = {key: result[key] for key in PUBLIC if key in result}
                        ownership = result.get("ownership_stop", {})
                        receipts[service]["ownership_stop"] = ownership
                        instance = confirm_tts_stop(result)
                        require(instance not in stopped_tts_instances, "tts_cycle_reused_owned_instance")
                        stopped_tts_instances.add(instance)
                except Exception as error:
                    errors.append(error)
                    failure(label + "_" + service, error)
            report["cleanup"][label] = receipts
            require(not errors, "speech_service_stop_failed")

        start_services()
        report["initial_warmup"] = wait_active(time.perf_counter() + method["initial_activation_timeout_seconds"])
        require(observed, "native_camera_worker_not_observed")
        measured = time.perf_counter()
        preview.measurement_started = measured
        report["initial_preparation_elapsed_seconds"] = measured - started
        offsets = cycle_offsets(args.duration_seconds, method["cycle_seconds"])
        report["expected_cycle_offsets_seconds"] = offsets
        cycle_index = 0
        while time.perf_counter() - measured < args.duration_seconds:
            if cycle_index < len(offsets) and time.perf_counter() - measured >= offsets[cycle_index]:
                cycle = {"scheduled_offset_seconds": offsets[cycle_index], "status": "started",
                         "started_offset_seconds": time.perf_counter() - measured}
                report["cycles"].append(cycle)
                stop_services("cycle_" + str(cycle_index + 1))
                start_services()
                next_offset = offsets[cycle_index + 1] if cycle_index + 1 < len(offsets) else args.duration_seconds
                cycle["warmup"] = wait_active(measured + next_offset)
                require(time.perf_counter() < measured + next_offset, "warm_cycle_overran_next_boundary")
                cycle["completed_offset_seconds"] = time.perf_counter() - measured
                cycle["status"] = "completed"
                cycle_index += 1
            else:
                warm_receipt(observe(), args.engine)
                time.sleep(min(method["state_poll_seconds"], max(0, args.duration_seconds - (time.perf_counter() - measured))))
        require(cycle_index == len(offsets), "scheduled_warm_cycles_not_completed")
        report["observed_measurement_seconds"] = time.perf_counter() - measured
        report["checks_passed"] = True
    except (Exception, KeyboardInterrupt) as error:
        failure("coexistence", error)
    finally:
        if preview is not None:
            try:
                preview.finish()
                report["video"] = preview.summarize()
                require(preview.error is None and preview.completed, "video_receive_failed")
            except (Exception, KeyboardInterrupt) as error:
                failure("video_cleanup", error)
        if samples_file is not None:
            samples_file.close()
        if app is not None:
            if app.process.poll() is None and app.token is not None:
                # These exact handles belong to this fixture. No global service
                # descriptor, port kill or WSL terminate/shutdown is used.
                for service, timeout in (("local-asr", 15), ("local-speech", 40)):
                    try:
                        result = app.call("POST", f"/api/audio/{service}/stop", {}, timeout=timeout)
                        if service == "local-asr":
                            report["cleanup"]["final_asr"] = asr_projection(result)
                            instance = confirm_stop(result)
                            if report.get("checks_passed"):
                                require(instance not in stopped_instances, "final_asr_reused_old_cycle")
                        else:
                            report["cleanup"]["final_tts"] = {key: result[key] for key in PUBLIC if key in result}
                            report["cleanup"]["final_tts"]["ownership_stop"] = result.get("ownership_stop")
                            instance = confirm_tts_stop(result)
                            if report.get("checks_passed"):
                                require(instance not in stopped_tts_instances, "final_tts_reused_old_cycle")
                    except (Exception, KeyboardInterrupt) as error:
                        failure(service + "_cleanup", error)
                try:
                    value = app.call("POST", "/api/camera/close", {})
                    require(value.get("state") == "closed", "camera_close_not_acknowledged")
                    until = time.perf_counter() + method["camera_worker_exit_timeout_seconds"]
                    while True:
                        observe_workers(app, executable, observed, psutil)
                        if not workers_alive(observed, psutil):
                            break
                        require(time.perf_counter() < until, "camera_worker_exit_unconfirmed")
                        time.sleep(.05)
                    report["cleanup"]["camera"] = {"observed_workers": len(observed), "alive": 0}
                    check_state(app.call("GET", "/api/state"))
                except (Exception, KeyboardInterrupt) as error:
                    failure("camera_cleanup", error)
            try:
                app.stop()
                require(report["main"].get("main_exit_code") == 0 and report["main"].get("shutdown_http_accepted")
                        and not report["main"].get("forced_owned_main_stop")
                        and report["main"].get("server_descriptor_removed_by_main"), "main_cleanup_unconfirmed")
            except (Exception, KeyboardInterrupt) as error:
                failure("main_cleanup", error)
        try:
            after = protected_hashes(root)
            report["protected_after_sha256"] = after
            require(protected is not None and after == protected, "protected_source_state_changed")
            require(sha256(executable) == report["executable_sha256"], "release_changed")
            for source, digest in sources.items():
                require(sha256(source) == digest, "source_asset_changed")
            report["source_assets_and_protected_files_unchanged"] = True
            require(not (fixture / "runtime/tts-service.json").exists()
                    and not (fixture / "runtime/speech-service.json").exists(), "unexpected_public_service_descriptor")
            if app is None or app.process.poll() is not None:
                for name, value in policies.items():
                    path = fixture / "runtime" / name
                    require(json.loads(path.read_text(encoding="utf-8")) in (value, {"protocol_version": 1, "mode": "disabled"},
                            {"protocol_version": 1, "mode": "disabled", "warmup": "disabled"}), "fixture_policy_changed")
                    path.unlink()
                selected = fixture / "runtime/rust_camera_device.txt"
                if selected.exists():
                    require(hashlib.sha256(selected.read_text(encoding="utf-8").encode()).hexdigest() == report.get("selected_device_id_sha256"),
                            "fixture_selected_camera_file_changed")
                    selected.unlink()
        except (Exception, KeyboardInterrupt) as error:
            failure("protected_files_or_fixture_cleanup", error)
        report["passed"] = bool(report.get("checks_passed") and not report["errors"])
        report["camera_workers_observed"] = [{"pid": pid, "created": created} for pid, created in observed]
        report["full_duration_completed"] = report["passed"] and report["run_kind"] == "full"
        report["elapsed_seconds"] = time.perf_counter() - started
        report["checked_at"] = datetime.datetime.now(datetime.timezone.utc).isoformat()
        write_json(output / "report.json", report)
        print(json.dumps({"passed": report["passed"], "run_kind": report["run_kind"],
                          "full_duration_completed": report["full_duration_completed"], "errors": report["errors"]}))
    return 0 if report["passed"] else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--real-camera-local-synthetic", action="store_true", required=True)
    for name in ("root", "exe", "ui", "python-executable", "output"):
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument("--engine", choices=("cosy", "qwen"), default="cosy")
    parser.add_argument("--distribution", default="Ubuntu")
    parser.add_argument("--references-manifest", type=Path)
    parser.add_argument("--duration-seconds", type=int, default=1800)
    camera = parser.add_mutually_exclusive_group(required=True)
    camera.add_argument("--device-id-file", type=Path)
    camera.add_argument("--single-camera", action="store_true")
    args = parser.parse_args()
    require(10 <= args.duration_seconds <= 1800, "duration_must_be_10_to_1800_seconds")
    require(args.distribution and len(args.distribution) <= 80, "explicit_distribution_invalid")
    return run(args)


if __name__ == "__main__":
    raise SystemExit(main())
