"""Continuous metadata-only receiver for the coexistence soak, not browser paint."""
import io
import json
from pathlib import Path
import threading
import time

from camera_stream_smoke import parse_packet, distribution, number
from database_compatibility_smoke import require

WIRE_METHOD = Path(__file__).with_name("camera_stream_method_v1.json")


class Preview:
    def __init__(self, app, output, connect, image, method):
        self.app, self.output, self.connect, self.image, self.method = app, output, connect, image, method
        self.ready, self.stop = threading.Event(), threading.Event()
        self.measurement_started = None
        self.error = None
        self.completed = False
        self.states = 0
        self.formats = []
        self.thread = threading.Thread(target=self.run, name="coexistence-preview", daemon=True)

    def start(self):
        self.thread.start()

    def finish(self):
        self.stop.set()
        self.thread.join(timeout=5)
        require(not self.thread.is_alive(), "preview_thread_exit_unconfirmed")

    def run(self):
        limits = json.loads(WIRE_METHOD.read_text(encoding="utf-8"))
        origin = f"http://127.0.0.1:{self.app.port}"
        anchor, wall_anchor = time.perf_counter(), time.time() * 1000
        delivery, previous_frame, epoch, previous_format = 0, None, None, None
        last_received = None
        try:
            with (self.output / "video-frames.jsonl").open("x", encoding="utf-8") as rows, self.connect(
                f"ws://127.0.0.1:{self.app.port}/api/video/stream", origin=origin,
                additional_headers={"Cookie": "ai_tutor_local=" + self.app.token}, proxy=None,
                compression=None, max_size=4 + limits["metadata_max_bytes"] + limits["jpeg_max_bytes"],
                max_queue=1, open_timeout=5, close_timeout=2, ping_interval=None,
            ) as ws:
                while not self.stop.is_set():
                    now = time.perf_counter()
                    require(last_received is not None or now - anchor < self.method["camera_first_frame_timeout_seconds"],
                            "first_camera_frame_timeout")
                    require(last_received is None or now - last_received < self.method["camera_stall_timeout_seconds"],
                            "camera_frame_stalled")
                    try:
                        packet = ws.recv(timeout=1)
                    except TimeoutError:
                        continue
                    received, wall = time.perf_counter(), time.time() * 1000
                    meta, jpeg = parse_packet(packet, limits, delivery)
                    delivery = meta["delivery_id"]
                    require(meta.get("selected_track_key") is None, "unexpected_camera_selection")
                    if not jpeg:
                        self.states += 1
                        continue
                    if epoch is None:
                        epoch = meta["camera_session_id"]
                    require(meta["camera_session_id"] == epoch, "unexpected_camera_restart")
                    frame_id = meta["frame_id"]
                    require(previous_frame is None or frame_id > previous_frame, "camera_frame_repeated_or_reversed")
                    started = time.perf_counter()
                    with self.image.open(io.BytesIO(jpeg)) as image:
                        require(image.format == "JPEG" and image.size == (meta["width"], meta["height"]),
                                "camera_jpeg_dimensions_invalid")
                        image.load()
                    decoded = time.perf_counter()
                    measuring = self.measurement_started
                    row = {"phase": "coexistence" if measuring is not None and received >= measuring else "preparation",
                           "elapsed_ms": (received - anchor) * 1000,
                           "frame_id": frame_id, "delivery_id": delivery,
                           "width": meta["width"], "height": meta["height"], "jpeg_bytes": len(jpeg),
                           "preview_jpeg_reused": meta.get("preview_jpeg_reused") is True,
                           "consumer_decode_ms": (decoded - started) * 1000,
                           "receive_to_decode_done_ms": (decoded - received) * 1000,
                           "capture_ids_skipped": 0 if previous_frame is None else frame_id - previous_frame - 1,
                           "confirmed_track_count": len(meta.get("tracks", []))}
                    for key in ("frame_age_ms", "capture_fps", "fps", "read_sample_ms", "decode_ms", "inference_ms", "processing_ms"):
                        if number(meta.get(key)):
                            row[key] = meta[key]
                    drift = (wall - wall_anchor) - (received - anchor) * 1000
                    captured = meta.get("captured_at_ms")
                    row["capture_age_clock_valid"] = number(captured) and wall >= captured and abs(drift) <= limits["clock_drift_limit_ms"]
                    if row["capture_age_clock_valid"]:
                        row["capture_return_to_receive_ms"] = wall - captured
                        row["capture_return_to_consumer_decode_done_ms"] = wall - captured + (decoded - received) * 1000
                    negotiated = meta.get("negotiated", {})
                    current_format = {"width": meta["width"], "height": meta["height"],
                                      **{key: negotiated.get(key) for key in ("pixel_format", "requested_fps", "driver_reported_fps")}}
                    require(isinstance(current_format["pixel_format"], str) and len(current_format["pixel_format"]) <= 32,
                            "camera_pixel_format_invalid")
                    if current_format != previous_format:
                        self.formats.append({"elapsed_ms": row["elapsed_ms"], "frame_id": frame_id, **current_format})
                        previous_format = current_format
                    ws.send(json.dumps({"protocol_version": 1, "type": "ack", "delivery_id": delivery,
                                        "frame_id": frame_id, "camera_session_id": epoch}, separators=(",", ":")))
                    row["receive_to_ack_sent_ms"] = (time.perf_counter() - received) * 1000
                    rows.write(json.dumps(row, separators=(",", ":")) + "\n")
                    rows.flush()
                    last_received, previous_frame = received, frame_id
                    self.ready.set()
                    del packet, meta, jpeg
            self.completed = True
        except Exception as error:
            from database_compatibility_smoke import SmokeFailure
            self.error = str(error) if isinstance(error, SmokeFailure) else type(error).__name__

    def summarize(self):
        phases = {}
        path = self.output / "video-frames.jsonl"
        if path.exists():
            with path.open(encoding="utf-8") as rows:
                for line in rows:
                    row = json.loads(line)
                    phase = phases.setdefault(row["phase"], {"frames": 0, "first_ms": row["elapsed_ms"],
                        "last_ms": row["elapsed_ms"], "capture_ids_skipped": 0, "clock_invalid_frames": 0,
                        "jpeg_reused_count": 0, "frames_with_tracks": 0, "samples": {}})
                    phase["frames"] += 1
                    phase["last_ms"] = row["elapsed_ms"]
                    phase["capture_ids_skipped"] += row["capture_ids_skipped"]
                    phase["clock_invalid_frames"] += int(not row["capture_age_clock_valid"])
                    phase["jpeg_reused_count"] += int(row["preview_jpeg_reused"])
                    phase["frames_with_tracks"] += int(row["confirmed_track_count"] > 0)
                    for key, value in row.items():
                        if number(value) and key not in ("frame_id", "delivery_id", "elapsed_ms", "capture_ids_skipped"):
                            phase["samples"].setdefault(key, []).append(value)
            for phase in phases.values():
                span = (phase["last_ms"] - phase["first_ms"]) / 1000
                phase["received_unique_fps"] = (phase["frames"] - 1) / span if span > 0 else None
                phase["distributions"] = {key: distribution(values) for key, values in phase.pop("samples").items()}
        return {"completed": self.completed, "error": self.error, "phases": phases,
                "format_observations": self.formats, "state_messages": self.states,
                "browser_decode_or_paint_measured": False, "raw_media_saved": False,
                "wire_method_sha_scope": "Uses unchanged camera_stream_method_v1 packet/size/age limits, not its 60/10/15 schedule"}
