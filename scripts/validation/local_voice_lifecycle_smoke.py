"""Explicit real-model preparation through an isolated main-process fixture.

No capture/playback request is sent. Retains the fixture, including read-only-use
hard links to fixed local model files; never recursively removes linked assets.
"""
import argparse
import hashlib
import http.client
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import time

BUNDLE = ("managed_tts.py", "tts_gpu_lease.sh", "cuda_link_env.sh",
          "start_qwen_omni.sh", "start_cosy_omni.sh", "qwen3_tts_12gb.yaml",
          "cosyvoice3_12gb_candidate.yaml")
STATUS_FIELDS = ("mode", "phase", "ready", "warmup_completed", "active", "busy",
                 "retry_available", "shutting_down", "instance_id", "engine_kind",
                 "managed_instance_owned", "ownership_start", "ownership_stop",
                 "linux_cleanup_confirmed", "client_deactivation_confirmed",
                 "receipt", "error")


def write_json(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2), encoding="utf-8")


def stage(root, output, ui, engine):
    output.mkdir(parents=True, exist_ok=False)
    fixture = output / "fixture"
    shutil.copytree(ui, fixture / "frontend/dist")
    bundle = fixture / "bin/local-speech"
    bundle.mkdir(parents=True)
    hashes = {}
    for name in BUNDLE:
        source = root / "scripts/speech-lab" / name
        destination = bundle / name
        shutil.copyfile(source, destination)
        hashes[name] = hashlib.sha256(destination.read_bytes()).hexdigest()
        assert hashes[name] == hashlib.sha256(source.read_bytes()).hexdigest()
    model = ("speech/Fun-CosyVoice3-0.5B-2512/omni-overlay" if engine == "cosy"
             else "speech/Qwen3-TTS-12Hz-0.6B-CustomVoice")
    source = root / "models" / model
    assert source.is_dir(), "fixed_local_model_missing"
    linked = []
    for file in source.rglob("*"):
        if file.is_file():
            relative = file.relative_to(root / "models")
            destination = fixture / "models" / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            os.link(file, destination)
            assert os.path.samefile(file, destination)
            linked.append(str(relative))
    pointer = fixture / "runtime/speech-gpu/omni-env-path.txt"
    pointer.parent.mkdir(parents=True)
    shutil.copyfile(root / "runtime/speech-gpu/omni-env-path.txt", pointer)
    config = {"engine": engine, "distribution": "Ubuntu"}
    if engine == "cosy":
        manifest = json.loads((root / "runtime/goal-baselines/20261104/cosyvoice3-preparation/reference-audio.json").read_text(encoding="utf-8"))
        references = {}
        for language in ("zh", "en"):
            row = next(row for row in manifest["samples"] if row["id"] == language + "_concept")
            file = Path(row["path"])
            assert file.is_absolute() and hashlib.sha256(file.read_bytes()).hexdigest() == row["sha256"]
            references[language] = {"wav_path": str(file), "sha256": row["sha256"], "text": row["text"]}
        write_json(fixture / "runtime/tts-references.json", references)
        config["references_file"] = "runtime/tts-references.json"
    policy = {"protocol_version": 1, "mode": "managed", "warmup": "synthetic_bilingual", "managed": config}
    write_json(fixture / "runtime/local-speech-policy.json", policy)
    return fixture, {"bundle_sha256": hashes, "model_files_hard_linked": linked,
                     "model_files_written_by_probe": False, "policy": policy}


def run(root, output, ui, engine, qwen_eager_lab=False):
    if qwen_eager_lab and engine != "qwen":
        raise ValueError("qwen_eager_lab_requires_qwen")
    fixture, staged = stage(root, output, ui, engine)
    if qwen_eager_lab:
        config = fixture / "bin/local-speech/qwen3_tts_12gb.yaml"
        before = config.read_bytes()
        assert hashlib.sha256(before).hexdigest() == "a79c0b71f391363df7a3dde0c4d5d6386315dfde997f7940449c96ddb451b9ea"
        text = before.decode("utf-8")
        assert text.count("  - stage_id: 0\n") == 1 and text.count("    enforce_eager: false\n") == 1
        text = text.replace("  - stage_id: 0\n", "  - stage_id: 0\n    enforce_eager: true\n")
        text = text.replace("    enforce_eager: false\n", "    enforce_eager: true\n")
        config.write_bytes(text.encode("utf-8"))
        staged["qwen_eager_lab"] = {
            "changed_only": "enforce_eager=true for both stages in the isolated fixture",
            "base_configuration_sha256": hashlib.sha256(before).hexdigest(),
            "configuration_sha256": hashlib.sha256(config.read_bytes()).hexdigest(),
            "method_frozen_before_model_start": True,
            "not_a_balanced_latency_or_listening_evaluation": True,
        }
        staged["bundle_sha256"]["qwen3_tts_12gb.yaml"] = staged["qwen_eager_lab"]["configuration_sha256"]
        write_json(output / "candidate-method.json", staged["qwen_eager_lab"])
    report = {"test_type": "main_owned_local_voice_synthetic", "success": False,
              "production_data_used": False, "production_published": False,
              "microphone_requested": False, "speaker_requested": False,
              "camera_requested": False, "ptz_requested": False, **staged,
              "transitions": []}
    with socket.socket() as reserved:
        reserved.bind(("127.0.0.1", 0))
        port = reserved.getsockname()[1]
    started = time.perf_counter()
    process = subprocess.Popen([str(root / "target/release/ai-tutor.exe"), "--root", str(fixture),
                                "--port", str(port), "--no-browser"],
                               creationflags=subprocess.CREATE_NO_WINDOW,
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    token = None

    def call(method, path, data=None, timeout=5):
        connection = http.client.HTTPConnection("127.0.0.1", port, timeout=timeout)
        try:
            connection.request(method, path, json.dumps(data) if data is not None else None,
                               {"Content-Type": "application/json", "Cookie": "ai_tutor_local=" + token})
            response = connection.getresponse()
            return response.status, json.loads(response.read())
        finally:
            connection.close()

    def api(method, path, data=None, timeout=5):
        status, value = call(method, path, data, timeout)
        assert status == 200, f"{path}: HTTP {status}"
        return value

    def state():
        value = api("GET", "/api/state")
        assert value["camera"]["state"] == "closed"
        assert value["ptz"]["mode"] == "off" and value["audio"]["recording"] is False
        local = value["local_speech"]
        snapshot = {key: local[key] for key in STATUS_FIELDS if key in local}
        snapshot["observed_ms"] = round((time.perf_counter() - started) * 1000, 3)
        snapshot["audio_configured_engine"] = value["audio"].get("tts_configured_engine")
        marker = (snapshot.get("phase"), snapshot.get("instance_id"), snapshot.get("busy"))
        previous = report["transitions"][-1] if report["transitions"] else {}
        if marker != (previous.get("phase"), previous.get("instance_id"), previous.get("busy")):
            report["transitions"].append(snapshot)
            write_json(output / "progress.json", {"transitions": report["transitions"]})
        return value

    try:
        descriptor = fixture / "runtime/server.json"
        deadline = time.monotonic() + 30
        while not descriptor.exists() and time.monotonic() < deadline and process.poll() is None:
            time.sleep(.05)
        info = json.loads(descriptor.read_text(encoding="utf-8"))
        assert info["pid"] == process.pid and info["port"] == port
        token = info["token"]
        # The descriptor is written just before the HTTP accept loop begins.
        connection_deadline = time.monotonic() + 10
        while True:
            try:
                state()
                break
            except (ConnectionRefusedError, ConnectionResetError):
                assert process.poll() is None and time.monotonic() < connection_deadline, "main_http_startup_failed"
                time.sleep(.05)
        deadline = time.monotonic() + 390
        while True:
            value = state()
            local = value["local_speech"]
            if local.get("active"):
                break
            assert local["phase"] not in ("degraded", "configuration_error", "cleanup_failed", "stopped"), "voice_failed_before_installation"
            assert time.monotonic() < deadline, "main_voice_preparation_deadline"
            time.sleep(.1)
        assert local["ready"] and local["warmup_completed"] and local["phase"] == "active"
        assert value["audio"]["tts_configured_engine"] == engine + "_pcm"
        report["first_active_ms"] = round((time.perf_counter() - started) * 1000, 3)
        report["active_receipt"] = {key: local[key] for key in STATUS_FIELDS if key in local}
        assert call("POST", "/api/audio/local-speech/retry", {})[0] == 400
        report["retry_while_running_rejected"] = True
        stop_started = time.perf_counter()
        stopped = api("POST", "/api/audio/local-speech/stop", {}, timeout=40)
        report["stop_ms"] = round((time.perf_counter() - stop_started) * 1000, 3)
        report["stop_receipt"] = {key: stopped[key] for key in STATUS_FIELDS if key in stopped}
        assert stopped["phase"] == "stopped" and stopped["linux_cleanup_confirmed"] is True
        assert stopped["client_deactivation_confirmed"] is True
        value = state()
        assert value["audio"].get("tts_configured_engine") is None
        assert not value["local_speech"]["busy"] and value["local_speech"]["retry_available"]
        assert api("POST", "/api/audio/local-speech/retry", {})["queued"] is True
        time.sleep(1)
        report["retry_cancel_phase"] = state()["local_speech"]["phase"]
        stopped = api("POST", "/api/audio/local-speech/stop", {}, timeout=40)
        report["retry_stop_receipt"] = {key: stopped[key] for key in STATUS_FIELDS if key in stopped}
        assert stopped["phase"] == "stopped" and stopped["active"] is False
        if "linux_cleanup_confirmed" in stopped:
            assert stopped["linux_cleanup_confirmed"] is True
        value = state()
        assert value["audio"].get("tts_configured_engine") is None
        assert not (fixture / "runtime/tts-service.json").exists()
        report["production_descriptor_written"] = False
        report["checks_passed"] = True
    except Exception as error:
        report["error"] = f"{type(error).__name__}: {error}"
    finally:
        if token and process.poll() is None:
            try:
                api("POST", "/api/shutdown", {}, timeout=10)
            except (OSError, AssertionError, http.client.HTTPException):
                report["shutdown_request_failed"] = True
        try:
            process.wait(timeout=35)
        except subprocess.TimeoutExpired:
            process.terminate()  # Exactly the main process created above; no name/PID search.
            process.wait(timeout=5)
            report["forced_owned_main_stop"] = True
            report["linux_cleanup_after_forced_main_stop"] = "unconfirmed"
        report["main_exit_code"] = process.returncode
        report["server_descriptor_removed"] = not (fixture / "runtime/server.json").exists()
        descriptor = fixture / "runtime/server.json"
        if descriptor.exists() and token:
            stale = json.loads(descriptor.read_text(encoding="utf-8"))
            if stale.get("pid") == process.pid and stale.get("token") == token:
                descriptor.unlink()
                report["stale_owned_credential_removed_by_probe"] = True
        # Retain model links/cache/evidence. Remove only this fixture's known policy.
        policy = fixture / "runtime/local-speech-policy.json"
        if policy.exists() and json.loads(policy.read_text(encoding="utf-8")) == staged["policy"]:
            policy.unlink()
            report["fixture_policy_removed"] = True
        report["success"] = bool(report.get("checks_passed") and process.returncode == 0
                                 and report["server_descriptor_removed"]
                                 and not report.get("forced_owned_main_stop"))
        report["elapsed_ms"] = round((time.perf_counter() - started) * 1000, 3)
        write_json(output / "report.json", report)
        print(json.dumps({"success": report["success"], "report": str(output / "report.json"),
                          "error": report.get("error"), "main_exit_code": process.returncode}))
    return report["success"]


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--real-local-synthetic", action="store_true", required=True)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--ui", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--engine", choices=("cosy", "qwen"), default="cosy")
    parser.add_argument("--qwen-eager-lab", action="store_true",
                        help="Use eager execution only in the new Qwen test fixture; never edit production configuration")
    args = parser.parse_args()
    raise SystemExit(0 if run(args.root.resolve(), args.output.resolve(), args.ui.resolve(), args.engine,
                             args.qwen_eager_lab) else 1)
