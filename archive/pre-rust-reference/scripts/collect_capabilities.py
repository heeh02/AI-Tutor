"""Assemble machine evidence without including credentials, frames or audio."""
import json
import platform
import sys
from pathlib import Path

root = Path(__file__).resolve().parents[1]


def load(relative):
    path = root / relative
    return json.loads(path.read_text(encoding="utf-8")) if path.exists() else None


camera = load("runtime/camera_reopen_probe.json") or load("runtime/camera_probe.json")
uvc = load("runtime/uvc_probe.json")
motion = load("runtime/ptz_motion_probe.json")
audio = load("runtime/audio-probe.json")
kimi = load("runtime/kimi/verification.json")
report = {"schema_version": "1.0", "date": "2026-11-01", "os": platform.platform(), "python": sys.version.split()[0],
          "camera": {"evidence": camera, "default_save_video": False, "identity_recognition": False,
                     "face_model": load("models/manifest.json"), "multi_person_physical_test": "NOT_RUN"},
          "ptz": {"native_read_probe": uvc, "motion_probe": motion, "mode": "off", "vendor_auto_state": "unknown", "external_yolo_enabled": False},
          "audio": {"real_microphone_probe": audio, "offline_synthetic_asr_probe": load("runtime/asr_probe.json"),
                    "asr_model": load("models/asr-manifest.json"), "human_voice_e2e": "NOT_RUN"},
          "kimi": {"version": "2.3.1", "tools": [], "subagents": [], "remote": True,
                   "verification_available": bool(kimi), "token_usage": None},
          "emotion": {"enabled": False}, "hardware_execution": {"enabled": False},
          "task_sources": ["learner_report", "operator_confirmed", "simulated_in_separate_database"],
          "camera_and_audio_sent_to_model": False}
(root / "DEVICE_CAPABILITIES.json").write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
print("DEVICE_CAPABILITIES.json updated (no frames/audio/credentials).")
