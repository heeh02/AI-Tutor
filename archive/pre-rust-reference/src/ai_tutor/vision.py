"""Local face-only vision. Frames stay in RAM; capture has a killable owner process."""
from __future__ import annotations

import ast
import hashlib
import multiprocessing as mp
from pathlib import Path
import queue
import threading
import time
import uuid

import cv2
import numpy as np

ROOT = Path(__file__).resolve().parents[2]
MODEL = ROOT / "models" / "yolov8n-face-lindevs.onnx"
MODEL_SHA256 = "8d0bfb0c3383c5bd7a78dd24ef79a21e2aa456619b6ab5e53867092d1c7dc414"


def discover_cameras():
    from cv2_enumerate_cameras import enumerate_cameras
    cameras = []
    for c in enumerate_cameras(cv2.CAP_DSHOW):
        stable_id = c.path or f"unstable-index:{c.index}"
        virtual = any(word in c.name.lower() for word in ("virtual", "obs virtual", "manycam"))
        cameras.append({"id": stable_id, "device_id": stable_id, "name": c.name,
                        "index": c.index, "backend": "DirectShow", "stable_id": bool(c.path),
                        "vid": c.vid, "pid": c.pid, "virtual": virtual,
                        "formats": "negotiated on open; full format enumeration unavailable"})
    return cameras


def _put_latest(output, item):
    try:
        output.put_nowait(item)
    except queue.Full:
        try:
            output.get_nowait()
        except queue.Empty:
            pass
        try:
            output.put_nowait(item)
        except queue.Full:
            pass


def _capture(device_id, output, stop):
    """Resolve native path each connection cycle; never rely on a remembered index."""
    cv2.setNumThreads(1)
    while not stop.is_set():
        cap = None
        try:
            devices = discover_cameras()
            dev = next((d for d in devices if d["id"] == device_id), None)
            if dev is None:
                raise RuntimeError("摄像头已断开；正在按设备标识重新发现")
            epoch = uuid.uuid4().hex[:12]
            cap = cv2.VideoCapture(dev["index"], cv2.CAP_DSHOW)
            if not cap.isOpened():
                raise RuntimeError("无法打开摄像头，请检查 Windows 相机权限或其他程序占用")
            cap.set(cv2.CAP_PROP_FOURCC, cv2.VideoWriter_fourcc(*"MJPG"))
            cap.set(cv2.CAP_PROP_FRAME_WIDTH, 1280)
            cap.set(cv2.CAP_PROP_FRAME_HEIGHT, 720)
            cap.set(cv2.CAP_PROP_FPS, 30)
            number, failures = 0, 0
            while not stop.is_set():
                ok, frame = cap.read()
                if not ok or frame is None:
                    failures += 1
                    if failures >= 5:
                        raise RuntimeError("摄像头读帧失败；正在释放并重连")
                    stop.wait(.05)
                    continue
                failures = 0
                number += 1
                _put_latest(output, {"kind": "frame", "frame": frame,
                    "frame_id": number, "captured_at": time.monotonic(), "camera_session_id": epoch,
                    "width": frame.shape[1], "height": frame.shape[0],
                    "negotiated_fps": cap.get(cv2.CAP_PROP_FPS), "device": dev})
        except Exception as exc:
            _put_latest(output, {"kind": "error", "error": str(exc)[:300]})
        finally:
            if cap is not None:
                cap.release()
        stop.wait(1)


def letterbox(image, size=640):
    h, w = image.shape[:2]
    ratio = min(size / w, size / h)
    nw, nh = round(w * ratio), round(h * ratio)
    left, top = (size - nw) // 2, (size - nh) // 2
    padded = np.full((size, size, 3), 114, np.uint8)
    padded[top:top + nh, left:left + nw] = cv2.resize(image, (nw, nh))
    blob = np.ascontiguousarray(padded[:, :, ::-1].transpose(2, 0, 1)[None], dtype=np.float32) / 255
    return blob, (ratio, left, top)


def unletterbox(boxes, transform, width, height):
    ratio, left, top = transform
    boxes = np.asarray(boxes, dtype=np.float32).copy().reshape(-1, 4)
    boxes[:, [0, 2]] = np.clip((boxes[:, [0, 2]] - left) / ratio, 0, width)
    boxes[:, [1, 3]] = np.clip((boxes[:, [1, 3]] - top) / ratio, 0, height)
    return boxes


def display_box(box, mirror=False):
    x1, y1, x2, y2 = box
    return [1 - x2, y1, 1 - x1, y2] if mirror else list(box)


class FaceDetector:
    def __init__(self, path=MODEL):
        import onnxruntime as ort
        if hashlib.sha256(Path(path).read_bytes()).hexdigest() != MODEL_SHA256:
            raise RuntimeError("人脸权重摘要不匹配；拒绝加载")
        options = ort.SessionOptions()
        options.intra_op_num_threads = 3
        options.inter_op_num_threads = 1
        self.session = ort.InferenceSession(str(path), sess_options=options,
                                           providers=["CPUExecutionProvider"])
        metadata = self.session.get_modelmeta().custom_metadata_map
        names = ast.literal_eval(metadata.get("names", "{}"))
        if names != {0: "face"}:
            raise RuntimeError(f"人脸类别不匹配: {names}")
        self.input = self.session.get_inputs()[0].name

    def detect(self, frame):
        import supervision as sv
        blob, transform = letterbox(frame)
        output = self.session.run(None, {self.input: blob})[0][0].T
        scores = output[:, 4]
        keep = scores >= .15  # retain low confidence detections for ByteTrack second association
        data, scores = output[keep], scores[keep]
        if not len(data):
            return sv.Detections.empty()
        xywh = data[:, :4].copy()
        xywh[:, :2] -= xywh[:, 2:] / 2
        indices = cv2.dnn.NMSBoxes(xywh.tolist(), scores.tolist(), .15, .45)
        if len(indices) == 0:
            return sv.Detections.empty()
        indices = np.asarray(indices).reshape(-1)
        boxes = xywh[indices]
        boxes[:, 2:] += boxes[:, :2]
        boxes = unletterbox(boxes, transform, frame.shape[1], frame.shape[0])
        return sv.Detections(xyxy=boxes, confidence=scores[indices],
                             class_id=np.zeros(len(indices), dtype=int))


class CameraService:
    def __init__(self):
        self._lock = threading.RLock()
        self._process = self._worker = self._stop = self._queue = None
        self._last = None
        self._selected = None
        self._selected_seen = 0
        self._state, self._error = "closed", None
        self._detector = None
        self._started = 0
        self._frames = 0
        self._device_id = None

    def discover(self):
        return discover_cameras()

    def open(self, device_id=None):
        self.close()
        devices = self.discover()
        if not devices:
            raise ValueError("未发现摄像头，请连接设备并检查 Windows 相机权限")
        if device_id is None:
            saved = ROOT / "runtime" / "camera_device.txt"
            previous = saved.read_text(encoding="utf8") if saved.exists() else None
            device = next((d for d in devices if d["id"] == previous), None)
            device = device or next((d for d in devices if "obsbot" in d["name"].lower() and not d["virtual"]), None)
            device = device or next((d for d in devices if not d["virtual"]), devices[0])
            device_id = device["id"]
        if not any(d["id"] == device_id for d in devices):
            raise ValueError("所选设备不存在；请刷新设备列表")
        (ROOT / "runtime").mkdir(exist_ok=True)
        (ROOT / "runtime" / "camera_device.txt").write_text(device_id, encoding="utf8")
        context = mp.get_context("spawn")
        self._queue = context.Queue(maxsize=1)
        self._stop = context.Event()
        self._state, self._error = "connecting", None
        self._last, self._selected = None, None
        self._started, self._frames = time.monotonic(), 0
        self._device_id = device_id
        self._process = context.Process(target=_capture, args=(device_id, self._queue, self._stop), daemon=True)
        self._process.start()
        self._worker = threading.Thread(target=self._infer, args=(self._stop,self._queue), daemon=True, name="face-inference")
        self._worker.start()
        return self.status()

    def _infer(self, stop, frames_queue):
        tracker, epoch = None, None
        import supervision as sv
        try:
            if self._detector is None:
                self._detector = FaceDetector()
            last_packet, restarts = time.monotonic(), 0
            first_frame = None
            while not stop.is_set():
                try:
                    packet = frames_queue.get(timeout=.25)
                except queue.Empty:
                    with self._lock:
                        if self._last and time.monotonic() - self._last["captured_at"] > 2:
                            self._state, self._error = "reconnecting", "画面已过期；正在重连"
                            self._selected = None
                    if time.monotonic() - last_packet > 8:
                        # Native driver calls can hang: terminate only this app's capture owner.
                        if restarts >= 2:
                            raise RuntimeError("摄像头驱动连续超时；请检查连接后重新打开")
                        process = self._process
                        if process and process.is_alive():
                            process.terminate()
                            process.join(timeout=2)
                        if stop.is_set():
                            break
                        context = mp.get_context("spawn")
                        self._process = context.Process(target=_capture,
                            args=(self._device_id, frames_queue, stop), daemon=True)
                        self._process.start()
                        restarts += 1
                        last_packet = time.monotonic()
                    continue
                last_packet = time.monotonic()
                if packet["kind"] == "error":
                    with self._lock:
                        self._state, self._error, self._selected = "reconnecting", packet["error"], None
                    continue
                if packet["camera_session_id"] != epoch:
                    epoch = packet["camera_session_id"]
                    # We process at most 15fps. Buffer is expressed in processed frame units.
                    tracker = sv.ByteTrack(track_activation_threshold=.4, lost_track_buffer=30,
                        minimum_matching_threshold=.8, frame_rate=15, minimum_consecutive_frames=3)
                    with self._lock:
                        self._selected = None
                tick = time.monotonic()
                detections = self._detector.detect(packet["frame"])
                detections = tracker.update_with_detections(detections)
                w, h = packet["width"], packet["height"]
                tracks = [{"track_key": f"{epoch}:{int(tid)}", "label": f"Track {int(tid)}",
                           "box": (box / np.array([w, h, w, h])).tolist(), "confidence": float(score)}
                          for box, score, tid in zip(detections.xyxy, detections.confidence, detections.tracker_id)]
                keys = {t["track_key"] for t in tracks}
                # Near-overlap makes association uncertain; never silently retain an ambiguous binding.
                ambiguous = any(_overlap(a["box"], b["box"]) > .25
                                for i, a in enumerate(tracks) for b in tracks[i + 1:])
                ok, jpeg = cv2.imencode(".jpg", packet.pop("frame"), [cv2.IMWRITE_JPEG_QUALITY, 78])
                if not ok:
                    continue
                with self._lock:
                    if ambiguous:
                        self._selected = None
                    if self._selected in keys:
                        self._selected_seen = time.monotonic()
                    if self._selected and time.monotonic() - self._selected_seen > .6:
                        self._selected = None
                    self._frames += 1
                    if first_frame is None:
                        first_frame = time.monotonic()
                    packet.update(jpeg=jpeg.tobytes(), tracks=tracks, inference_ms=round((time.monotonic()-tick)*1000, 1),
                                  fps=round(max(0,self._frames-1) / max(.01, time.monotonic()-first_frame), 2),
                                  association_ambiguous=ambiguous)
                    self._last = packet
                    self._state, self._error = "running", None
                stop.wait(max(0, 1/15 - (time.monotonic() - tick)))
        except Exception as exc:
            with self._lock:
                self._state, self._error = "error", f"人脸检测失败：{str(exc)[:250]}"
            stop.set()

    def select(self, track_key):
        with self._lock:
            data = self.latest()
            if not data or data["frame_age_ms"] > 500 or track_key not in {t["track_key"] for t in data["tracks"]}:
                raise ValueError("该目标已消失或画面过期，请重新选择")
            if data.get("association_ambiguous"):
                raise ValueError("目标相互遮挡，关联不确定；请分开后重新选择")
            self._selected, self._selected_seen = track_key, time.monotonic()
            return self.status()

    def clear(self):
        with self._lock:
            self._selected = None
            return self.status()

    def latest(self):
        with self._lock:
            if not self._last:
                return None
            data = dict(self._last)
            age = (time.monotonic() - data["captured_at"]) * 1000
            present = self._selected in {t["track_key"] for t in data["tracks"]}
            data.update(frame_age_ms=round(age, 1), selected_track_key=self._selected,
                        selected_status=("visible" if present and age < 400 else "temporarily_lost") if self._selected else "unselected")
            return data

    def status(self):
        with self._lock:
            data = self.latest() or {}
            return {"state": self._state, "error": self._error,
                    **{k: v for k, v in data.items() if k not in ("jpeg", "kind")},
                    "selected_track_key": data.get("selected_track_key"),
                    "selected_status": data.get("selected_status", "unselected"),
                    "tracker": "ByteTrack", "face_model": "lindevs/yolov8n-face",
                    "ptz": {"mode": "off", "status": "BLOCKED", "physical_movement_verified": False,
                            "reason": "Tiny 3 支持官方 SDK/OBSBOT Center OSC；本机未安装控制接口，需申请 SDK 或安装并启用 Center OSC"}}

    def close(self):
        if self._stop:
            self._stop.set()
        if self._process:
            self._process.join(timeout=2)
            if self._process.is_alive():
                self._process.terminate()
                self._process.join(timeout=2)
        if self._worker:
            self._worker.join(timeout=3)
        if self._queue:
            self._queue.cancel_join_thread()
            self._queue.close()
        self._process = self._worker = self._stop = self._queue = None
        with self._lock:
            self._last = self._selected = None
            self._state, self._error = "closed", None


def _overlap(a, b):
    intersection = max(0, min(a[2],b[2])-max(a[0],b[0])) * max(0,min(a[3],b[3])-max(a[1],b[1]))
    area_a, area_b = (a[2]-a[0])*(a[3]-a[1]), (b[2]-b[0])*(b[3]-b[1])
    return intersection / max(1e-8, min(area_a, area_b))
