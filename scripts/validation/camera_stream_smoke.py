"""Explicit real-camera diagnostic through the existing native main and WS route.

No import-time execution. JPEGs exist only in memory. This consumer's decode is
not the Yew/browser decoder and its ACK never claims browser paint.
"""
import argparse
import datetime
import hashlib
import importlib.metadata
import io
import json
import math
import os
from pathlib import Path
import shutil
import struct
import time

from database_compatibility_smoke import (
    OwnedWorkstation, SmokeFailure, isolated_environment, require, sha256, write_json,
)

METHOD = Path(__file__).with_name("camera_stream_method_v1.json")
ASSETS = {
    "models/yolov8n-face-lindevs.onnx": "8d0bfb0c3383c5bd7a78dd24ef79a21e2aa456619b6ab5e53867092d1c7dc414",
    "models/native/onnxruntime.dll": "2bc04b05efbff97049132939640963772cc16225ba1c7f1388993487c8775492",
    "models/native/onnxruntime_providers_shared.dll": "6adee08db23ba190fd1061ee99c32a5a802cac1e3576defaac820c157294c138",
}
PROTECTED = (
    "bin/ai-tutor.exe",
    "runtime/local-asr-policy.json", "runtime/local-speech-policy.json", "runtime/tutor-delivery.json",
    "runtime/tts-service.json", "runtime/speech-service.json", "runtime/ptz-osc-config.json",
    "runtime/ptz-calibration-rust.json", "runtime/stop-fault.json", "runtime/rust_camera_device.txt",
)
ALLOWED = {("GET", "/api/state"), ("GET", "/api/devices/cameras"),
           ("POST", "/api/camera/open"), ("POST", "/api/camera/close"), ("POST", "/api/shutdown")}
NUMBERS = ("frame_age_ms", "capture_fps", "fps", "read_sample_ms", "decode_ms", "inference_ms",
           "processing_ms", "native_pose_last_read_ms", "selected_last_seen_age_ms")
TIMINGS = ("preprocess", "ort_run", "output_decode", "nms", "tracking", "association_metadata",
           "jpeg_encode", "preview_prepare", "latest_slot_wait", "capture_to_process")


class VideoWorkstation(OwnedWorkstation):
    def call(self, method, path, body=None):
        require((method, path) in ALLOWED, "diagnostic_endpoint_not_allowed")
        return super().call(method, path, body)


def protected_hashes(root):
    return {name: sha256(root / name) if (root / name).is_file() else None for name in PROTECTED}


def number(value):
    return isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value)


def parse_packet(data, method, previous_delivery):
    require(isinstance(data, bytes) and len(data) >= 4, "ws_binary_packet_required")
    meta_length = struct.unpack(">I", data[:4])[0]
    require(0 < meta_length <= method["metadata_max_bytes"] and 4 + meta_length <= len(data),
            "metadata_length_invalid")
    meta = json.loads(data[4:4 + meta_length].decode("utf-8"))
    require(isinstance(meta, dict) and meta.get("protocol_version") == 1, "metadata_protocol_invalid")
    delivery = meta.get("delivery_id")
    require(type(delivery) is int and previous_delivery < delivery <= 2**53 - 1,
            "delivery_id_not_strictly_increasing")
    jpeg = data[4 + meta_length:]
    require(type(meta.get("jpeg_length")) is int and meta["jpeg_length"] == len(jpeg), "jpeg_length_invalid")
    kind = meta.get("message_type")
    require(kind in ("frame", "state"), "message_type_invalid")
    if kind == "state":
        require(not jpeg, "state_contains_pixels")
    else:
        require(0 < len(jpeg) <= method["jpeg_max_bytes"], "jpeg_length_over_limit")
        require(type(meta.get("frame_id")) is int and 0 < meta["frame_id"] <= 2**53 - 1, "frame_id_invalid")
        epoch = meta.get("camera_session_id")
        require(isinstance(epoch, str) and 0 < len(epoch.encode("utf-8")) <= 128, "camera_epoch_invalid")
        require(meta.get("frame_age_origin") == "after_camera_frame_return", "age_origin_changed")
        width, height = meta.get("width"), meta.get("height")
        require(type(width) is int and type(height) is int and width > 0 and height > 0
                and width * height * 3 <= method["decoded_rgb_allocation_max_bytes"], "image_dimensions_invalid")
    return meta, jpeg


def distribution(values):
    values = sorted(value for value in values if number(value))
    if not values:
        return {"count": 0}
    return {"count": len(values), "min": values[0], "max": values[-1],
            "mean": sum(values) / len(values),
            **{name: values[max(0, math.ceil(len(values) * quantile) - 1)]
               for name, quantile in (("p50", .5), ("p95", .95), ("p99", .99))}}


def check_isolation(state):
    require(state["audio"].get("recording") is False and state["audio"].get("state") == "idle",
            "unexpected_audio_activity")
    require(state["ptz"].get("mode") == "off" and state["ptz"].get("commands_sent") == 0
            and state["ptz"].get("command_attempts") == 0
            and state["ptz"].get("hardware_stop_command_sent") is False, "unexpected_ptz_activity")
    require(state["kimi"].get("available") is False and state["kimi"].get("running", 0) == 0
            and state["kimi"]["metrics"].get("requests", 0) == 0, "unexpected_remote_model_activity")
    require(state["camera"].get("selected_track_key") is None, "unexpected_selected_target")
    for field in ("local_asr", "local_speech"):
        require(state[field].get("phase") == "disabled" and state[field].get("active") is False,
                "unexpected_local_speech_service")


def observe_workers(app, executable, observed, psutil):
    if app.process.poll() is not None:
        return
    for child in psutil.Process(app.process.pid).children(recursive=False):
        try:
            args = child.cmdline()
            if len(args) != 4 or args[1] != "--camera-worker":
                continue
            require(Path(child.exe()).samefile(executable) and Path(args[2]).samefile(app.fixture),
                    "camera_worker_owner_mismatch")
            observed[(child.pid, child.create_time())] = child
        except psutil.NoSuchProcess:
            continue


def workers_alive(observed, psutil):
    alive = []
    for (pid, created), process in observed.items():
        try:
            if process.is_running() and process.create_time() == created:
                alive.append(pid)
        except psutil.NoSuchProcess:
            pass
    return alive


def close_camera(app, executable, observed, psutil, method):
    observe_workers(app, executable, observed, psutil)
    result = app.call("POST", "/api/camera/close", {})
    require(result.get("state") == "closed", "camera_close_not_acknowledged")
    deadline = time.monotonic() + method["worker_exit_timeout_seconds"]
    while time.monotonic() < deadline:
        observe_workers(app, executable, observed, psutil)
        if not workers_alive(observed, psutil):
            state = app.call("GET", "/api/state")
            check_isolation(state)
            require(state["camera"].get("state") == "closed", "camera_not_closed")
            return {"camera_closed": True, "observed_worker_count": len(observed), "workers_alive": 0}
        time.sleep(.05)
    raise SmokeFailure("owned_camera_worker_still_alive_after_close")


def consume(app, executable, output, method, phase, seconds, delay_ms, observed, deps,
            result, expected_epoch=None, forbidden_epoch=None, open_started=None):
    connect, Image, psutil = deps
    result.update(status="running", requested_seconds=seconds, requested_ack_delay_ms=delay_ms,
                  frames=0, state_messages=0, clock_invalid_frames=0, tracks={}, distributions={},
                  format_observations=[], preview_jpeg_reused_count=0, preview_jpeg_reencoded_count=0)
    samples = {}
    previous_format = None
    first = last = None
    previous_id = None
    skipped = 0
    epoch = None
    delivery = 0
    anchor_wall, anchor_mono = time.time() * 1000, time.perf_counter()
    waiting_deadline = anchor_mono + method["first_frame_timeout_seconds"]
    next_health = anchor_mono
    path = output / (phase + "-frames.jsonl")
    try:
        with path.open("x", encoding="utf-8") as rows, connect(
            f"ws://127.0.0.1:{app.port}/api/video/stream", origin=f"http://127.0.0.1:{app.port}",
            additional_headers={"Cookie": "ai_tutor_local=" + app.token}, proxy=None,
            compression=None, max_size=4 + method["metadata_max_bytes"] + method["jpeg_max_bytes"],
            max_queue=1, open_timeout=5, close_timeout=2, ping_interval=None,
        ) as ws:
            while first is None or time.perf_counter() - first < seconds:
                now = time.perf_counter()
                require(first is not None or now < waiting_deadline, "first_frame_timeout")
                require(last is None or now - last < 3, "frame_stream_stalled")
                if now >= next_health:
                    check_isolation(app.call("GET", "/api/state"))
                    observe_workers(app, executable, observed, psutil)
                    next_health = now + 1
                try:
                    packet = ws.recv(timeout=min(1, max(.01, waiting_deadline - now)) if first is None else 1)
                except TimeoutError:
                    require(first is not None or time.perf_counter() < waiting_deadline, "first_frame_timeout")
                    require(last is None or time.perf_counter() - last < 3, "frame_stream_stalled")
                    continue
                received, wall = time.perf_counter(), time.time() * 1000
                meta, jpeg = parse_packet(packet, method, delivery)
                delivery = meta["delivery_id"]
                frame_delivery = delivery
                require(meta.get("selected_track_key") is None, "stream_selected_target_unexpected")
                if not jpeg:
                    result["state_messages"] += 1
                    continue
                current_epoch = meta["camera_session_id"]
                if epoch is None:
                    require(expected_epoch is None or current_epoch == expected_epoch, "epoch_changed_between_consumers")
                    require(forbidden_epoch is None or current_epoch != forbidden_epoch, "reopen_reused_old_epoch")
                    epoch = current_epoch
                    first = received
                    result["camera_epoch_sha256"] = hashlib.sha256(epoch.encode()).hexdigest()
                    result["width"], result["height"] = meta["width"], meta["height"]
                    negotiated = meta.get("negotiated", {})
                    result["negotiated_numeric"] = {key: negotiated[key] for key in
                        ("width", "height", "requested_fps", "driver_reported_fps") if number(negotiated.get(key))}
                    result["first_frame_wait_ms"] = (first - anchor_mono) * 1000
                    if open_started is not None:
                        result["open_request_to_first_received_ms"] = (first - open_started) * 1000
                require(current_epoch == epoch, "unexpected_camera_restart")
                frame_id = meta["frame_id"]
                require(previous_id is None or frame_id > previous_id, "duplicate_or_reversed_frame")
                if previous_id is not None:
                    skipped += frame_id - previous_id - 1
                previous_id = frame_id
                result["last_received_frame_id"] = frame_id
                result["last_stage"] = "decoding"
                decode_started = time.perf_counter()
                with Image.open(io.BytesIO(jpeg)) as image:
                    require(image.format == "JPEG" and image.size == (meta["width"], meta["height"]),
                            "jpeg_header_dimensions_mismatch")
                    image.load()
                decoded = time.perf_counter()
                # No JPEG, image object, boxes, names, or native device/pose data
                # are written. Each row is a strict numerical projection.
                row = {"phase": phase, "frame_id": frame_id, "delivery_id": delivery,
                       "elapsed_ms": (received - first) * 1000, "jpeg_bytes": len(jpeg),
                       "width": meta["width"], "height": meta["height"],
                       "preview_jpeg_reused": meta.get("preview_jpeg_reused") is True,
                       "consumer_decode_ms": (decoded - decode_started) * 1000,
                       "receive_to_decode_done_ms": (decoded - received) * 1000}
                negotiated = meta.get("negotiated", {})
                current_format = {"width": meta["width"], "height": meta["height"],
                                  "pixel_format": negotiated.get("pixel_format")}
                require(isinstance(current_format["pixel_format"], str)
                        and 0 < len(current_format["pixel_format"]) <= 32, "pixel_format_missing_or_invalid")
                for key in ("requested_fps", "driver_reported_fps"):
                    if number(negotiated.get(key)):
                        current_format[key] = negotiated[key]
                if current_format != previous_format:
                    result["format_observations"].append({"frame_id": frame_id,
                        "elapsed_ms": row["elapsed_ms"], **current_format})
                    previous_format = current_format
                counter = "preview_jpeg_reused_count" if row["preview_jpeg_reused"] else "preview_jpeg_reencoded_count"
                result[counter] += 1
                for key in NUMBERS:
                    if number(meta.get(key)):
                        row[key] = meta[key]
                for key in TIMINGS:
                    value = meta.get("timings_ms", {}).get(key)
                    if number(value):
                        row["worker_" + key + "_ms"] = value
                drift = (wall - anchor_wall) - (received - anchor_mono) * 1000
                row["wall_monotonic_drift_ms"] = drift
                captured = meta.get("captured_at_ms")
                clock_ok = number(captured) and abs(drift) <= method["clock_drift_limit_ms"] and wall >= captured
                row["capture_age_clock_valid"] = clock_ok
                if clock_ok:
                    row["capture_return_to_receive_ms"] = wall - captured
                    row["capture_return_to_consumer_decode_done_ms"] = wall - captured + (decoded - received) * 1000
                else:
                    result["clock_invalid_frames"] += 1
                tracks = meta.get("tracks", [])
                require(isinstance(tracks, list), "tracks_not_array")
                row["confirmed_track_count"] = len(tracks)
                row["scene_association_ambiguous"] = meta.get("scene_association_ambiguous") is True
                for track in tracks:
                    key = track.get("track_key")
                    require(isinstance(key, str) and key, "track_key_invalid")
                    alias = hashlib.sha256(key.encode()).hexdigest()
                    life = result["tracks"].setdefault(alias, {"frames_observed": 0,
                        "first_frame_id": frame_id, "last_frame_id": frame_id,
                        "first_elapsed_ms": row["elapsed_ms"], "last_elapsed_ms": row["elapsed_ms"],
                        "largest_observation_gap_ms": 0, "ambiguous_observations": 0})
                    life["frames_observed"] += 1
                    life["last_frame_id"] = frame_id
                    life["largest_observation_gap_ms"] = max(life["largest_observation_gap_ms"], row["elapsed_ms"] - life["last_elapsed_ms"])
                    life["last_elapsed_ms"] = row["elapsed_ms"]
                    life["ambiguous_observations"] += int(track.get("association_ambiguous") is True)
                if delay_ms:
                    # Continue reading control/state during the intentional delay.
                    # A second JPEG before this exact ACK violates the one-slot contract.
                    ack_at = time.perf_counter() + delay_ms / 1000
                    while time.perf_counter() < ack_at:
                        try:
                            pending_packet = ws.recv(timeout=max(.001, ack_at - time.perf_counter()))
                        except TimeoutError:
                            break
                        pending_meta, pending_jpeg = parse_packet(pending_packet, method, delivery)
                        delivery = pending_meta["delivery_id"]
                        require(not pending_jpeg, "second_jpeg_before_ack")
                        require(pending_meta.get("selected_track_key") is None, "stream_selected_target_unexpected")
                        result["state_messages"] += 1
                        result["states_while_ack_pending"] = result.get("states_while_ack_pending", 0) + 1
                result["last_stage"] = "acknowledging_decoded_frame"
                ws.send(json.dumps({"protocol_version": 1, "type": "ack", "delivery_id": frame_delivery,
                                    "frame_id": frame_id, "camera_session_id": epoch}, separators=(",", ":")))
                row["receive_to_ack_sent_ms"] = (time.perf_counter() - received) * 1000
                rows.write(json.dumps(row, separators=(",", ":")) + "\n")
                rows.flush()
                for key, value in row.items():
                    if number(value) and key not in ("frame_id", "delivery_id", "elapsed_ms"):
                        samples.setdefault(key, []).append(value)
                result["frames"] += 1
                last = received
                del jpeg, packet, meta
        require(result["frames"] >= 2 and observed, "insufficient_frames_or_no_owned_worker_observed")
        require(result["clock_invalid_frames"] < result["frames"], "no_valid_capture_age_measurements")
        result["status"] = "completed"
    finally:
        result["distributions"] = {key: distribution(values) for key, values in samples.items()}
        result["capture_ids_not_delivered_to_this_consumer"] = skipped
        result["observed_receive_span_seconds"] = last - first if first is not None and last is not None else None
        result["received_unique_fps"] = (result["frames"] - 1) / (last - first) if first is not None and last is not None and last > first else None
        result["selected_target_evaluated"] = False
        result["browser_decode_or_paint_measured"] = False
        if result["status"] == "running":
            result["status"] = "failed_partial_results_retained"
    return epoch


def run(args):
    root, executable, ui, output = (p.resolve() for p in (args.root, args.exe, args.ui, args.output))
    require(os.name == "nt" and args.real_camera, "explicit_windows_real_camera_required")
    require(executable.is_file() and (ui / "index.html").is_file(), "explicit_release_or_ui_missing")
    require(not output.exists() and not output.is_relative_to(ui), "output_must_be_fresh_outside_ui")
    output.mkdir(parents=True)
    fixture = output / "fixture"
    method = json.loads(METHOD.read_text(encoding="utf-8"))
    write_json(output / "method.json", method)
    report = {"method_id": method["method_id"], "method_sha256": sha256(METHOD),
              "script_sha256": sha256(Path(__file__)), "executable_sha256": sha256(executable),
              "helper_sha256": sha256(Path(__file__).with_name("database_compatibility_smoke.py")),
              "passed": False, "production_database_opened": False, "raw_media_saved": False,
              "audio_api_calls": 0, "target_selection_api_calls": 0, "ptz_api_calls": 0,
              "physical_gimbal_stationarity_claimed": False, "browser_used": False,
              "performance_gate": None, "owner": {}, "assets": {}, "closes": [],
              "phases": [{"id": item["id"], "status": "not_run"} for item in method["phases"]]}
    owned, observed, protected_before, policies = None, {}, None, {}
    checks_ok = False
    try:
        dependencies = {name: importlib.metadata.version(name) for name in ("websockets", "Pillow", "psutil")}
        require(dependencies == {"websockets": "15.0.1", "Pillow": "12.3.0", "psutil": "7.0.0"}, "diagnostic_dependency_version_changed")
        report["diagnostic_dependencies"] = dependencies
        from websockets.sync.client import connect
        from PIL import Image
        import psutil
        deps = (connect, Image, psutil)
        protected_before = protected_hashes(root)
        report["protected_before_sha256"] = protected_before
        shutil.copytree(ui, fixture / "frontend/dist")
        for name, expected in ASSETS.items():
            source, target = root / name, fixture / name
            require(source.is_file() and sha256(source) == expected, "fixed_vision_asset_hash_mismatch")
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, target)
            require(sha256(target) == expected, "staged_vision_asset_hash_mismatch")
            report["assets"][name] = {"sha256": expected, "bytes": target.stat().st_size}
        runtime = fixture / "runtime"
        runtime.mkdir()
        policies = {"local-asr-policy.json": {"protocol_version": 1, "mode": "disabled"},
                    "local-speech-policy.json": {"protocol_version": 1, "mode": "disabled", "warmup": "disabled"}}
        for name, value in policies.items():
            write_json(runtime / name, value)
        owned = VideoWorkstation(executable, fixture, isolated_environment(fixture), report["owner"])
        check_isolation(owned.ready())
        devices = owned.call("GET", "/api/devices/cameras")
        require(isinstance(devices, list), "camera_catalog_not_array")
        report["enumerated_camera_count"] = len(devices)
        if args.device_id_file:
            device = args.device_id_file.read_text(encoding="utf-8-sig").strip()
            require(sum(row.get("device_id") == device for row in devices) == 1, "explicit_camera_not_uniquely_present")
        else:
            require(args.single_camera and len(devices) == 1, "single_camera_required_or_explicit_id")
            device = devices[0].get("device_id")
        require(isinstance(device, str) and device, "camera_id_missing")
        report["selected_device_id_sha256"] = hashlib.sha256(device.encode()).hexdigest()
        first_open = time.perf_counter()
        owned.call("POST", "/api/camera/open", {"device_id": device})
        epoch = consume(owned, executable, output, method, "normal", args.seconds, 0, observed, deps,
                        report["phases"][0], open_started=first_open)
        consume(owned, executable, output, method, "slow_ack", args.slow_seconds, 150, observed, deps,
                report["phases"][1], expected_epoch=epoch)
        report["closes"].append(close_camera(owned, executable, observed, psutil, method))
        workers_before_reopen = len(observed)
        reopened = time.perf_counter()
        owned.call("POST", "/api/camera/open", {"device_id": device})
        consume(owned, executable, output, method, "reopened", args.reopen_seconds, 0, observed, deps,
                report["phases"][2], forbidden_epoch=epoch, open_started=reopened)
        report["closes"].append(close_camera(owned, executable, observed, psutil, method))
        require(len(observed) > workers_before_reopen, "reopened_worker_not_independently_observed")
        checks_ok = True
    except Exception as error:
        report["error_code"] = str(error) if isinstance(error, SmokeFailure) else "diagnostic_" + type(error).__name__
    finally:
        if owned is not None:
            try:
                if owned.process.poll() is None:
                    report["closes"].append(close_camera(owned, executable, observed, psutil, method))
            except Exception as error:
                report["camera_cleanup_error"] = str(error) if isinstance(error, SmokeFailure) else "camera_cleanup_exception"
            try:
                owned.stop()
                require(report["owner"].get("exit_code") == 0 and not report["owner"].get("forced_owned_process_stop")
                        and report["owner"].get("shutdown_http_accepted"), "owned_main_shutdown_not_clean")
                require(not workers_alive(observed, psutil), "worker_alive_after_main_exit")
                report["worker_cleanup_confirmed"] = bool(observed)
                require(not owned.descriptor.exists(), "owned_descriptor_remained")
            except Exception as error:
                report["main_cleanup_error"] = str(error) if isinstance(error, SmokeFailure) else "main_cleanup_exception"
        try:
            report["protected_after_sha256"] = protected_hashes(root)
            require(protected_before is not None and protected_before == report["protected_after_sha256"], "protected_policy_or_descriptor_changed")
            require(sha256(executable) == report["executable_sha256"], "executable_changed_during_diagnostic")
            for name, expected in ASSETS.items():
                require(sha256(root / name) == expected, "source_vision_asset_changed")
            for name, value in policies.items():
                require(json.loads((fixture / "runtime" / name).read_text(encoding="utf-8")) == value, "fixture_policy_changed")
            report["protected_files_unchanged"] = True
        except Exception as error:
            report["protection_error"] = str(error) if isinstance(error, SmokeFailure) else "protection_check_exception"
        report["worker_processes_observed"] = [{"pid": pid, "created": created} for pid, created in observed]
        if owned is not None and owned.process.poll() is not None:
            selected_file = fixture / "runtime/rust_camera_device.txt"
            try:
                if selected_file.is_file():
                    require(hashlib.sha256(selected_file.read_text(encoding="utf-8").encode()).hexdigest()
                            == report.get("selected_device_id_sha256"), "fixture_selection_changed")
                    selected_file.unlink()  # Only the new fixture's verified camera selection.
            except Exception:
                report["camera_cleanup_error"] = "fixture_selection_cleanup_unconfirmed"
        report["passed"] = checks_ok and not any(report.get(key) for key in
            ("error_code", "camera_cleanup_error", "main_cleanup_error", "protection_error"))
        report["checked_at"] = datetime.datetime.now(datetime.timezone.utc).isoformat()
        write_json(output / "report.json", report)
        print(json.dumps({"passed": report["passed"], "phases": [p["status"] for p in report["phases"]]}))
    return 0 if report["passed"] else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--real-camera", action="store_true", required=True)
    for name in ("root", "exe", "ui", "output"):
        parser.add_argument("--" + name, type=Path, required=True)
    selection = parser.add_mutually_exclusive_group(required=True)
    selection.add_argument("--device-id-file", type=Path)
    selection.add_argument("--single-camera", action="store_true")
    parser.add_argument("--seconds", type=float, default=60)
    parser.add_argument("--slow-seconds", type=float, default=10)
    parser.add_argument("--reopen-seconds", type=float, default=15)
    args = parser.parse_args()
    for seconds in (args.seconds, args.slow_seconds, args.reopen_seconds):
        parser.error("phase durations must be 2..300 seconds") if not 2 <= seconds <= 300 else None
    return run(args)


if __name__ == "__main__":
    raise SystemExit(main())
