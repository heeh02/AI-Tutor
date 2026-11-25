"""Run a real, non-recording camera/face probe. No images or video are written."""
import argparse
import json
from pathlib import Path
import statistics
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))
from ai_tutor.vision import CameraService


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--seconds", type=float, default=12)
    parser.add_argument("--out", default="runtime/camera_probe.json")
    args = parser.parse_args()
    service = CameraService()
    report = {"test": "real_camera_face_probe", "devices": service.discover(),
              "raw_media_saved": False, "date": "2026-11-01", "sample_count": 0}
    samples, seen_frames = [], set()
    max_faces = 0
    try:
        service.open()
        deadline = time.monotonic() + args.seconds
        while time.monotonic() < deadline:
            data = service.latest()
            if data and (data["camera_session_id"], data["frame_id"]) not in seen_frames:
                seen_frames.add((data["camera_session_id"], data["frame_id"]))
                samples.append({"age_ms": data["frame_age_ms"], "inference_ms": data["inference_ms"],
                                "frame_id": data["frame_id"], "tracks": len(data["tracks"])})
                max_faces = max(max_faces, len(data["tracks"]))
            time.sleep(.04)
        report["status"] = service.status()
        report["result"] = "PASS" if len(samples) >= 20 else "FAIL"
        report["sample_count"] = len(samples)
        report["max_faces_observed"] = max_faces
        if samples:
            report["median_frame_age_ms"] = statistics.median(s["age_ms"] for s in samples)
            report["median_inference_ms"] = statistics.median(s["inference_ms"] for s in samples)
            report["p95_frame_age_ms"] = sorted(s["age_ms"] for s in samples)[int((len(samples)-1)*.95)]
    finally:
        service.close()
        report["released"] = service.status()["state"] == "closed"
    Path(args.out).parent.mkdir(parents=True, exist_ok=True)
    Path(args.out).write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf8")
    print(json.dumps(report, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
