"""Pure coexistence receipt/schedule/report tests. No startup, model or devices."""
import copy
import json
from pathlib import Path
import tempfile
import unittest

from camera_speech_coexistence_soak import ALLOWED, cycle_offsets, warm_receipt, confirm_tts_stop
from coexistence_preview import Preview
from database_compatibility_smoke import SmokeFailure


def active_state():
    active = {"phase": "active", "active": True, "ready": True, "warmup_completed": True}
    return {"local_asr": active.copy(), "local_speech": {**active, "receipt": {"preparation_ms": 10,
        "languages": [{"language": language, "samples": 480, "transport": {"samples": 480}}
                      for language in ("zh", "en")]}},
        "audio": {"asr_ready": True, "tts_configured_engine": "cosy_pcm"}}


class CoexistenceTests(unittest.TestCase):
    def test_full_schedule_has_five_nonoverlapping_cycles_and_short_does_not_claim_them(self):
        self.assertEqual(cycle_offsets(1800, 300), [300, 600, 900, 1200, 1500])
        self.assertEqual(cycle_offsets(60, 300), [])
        self.assertEqual(cycle_offsets(600, 300), [300])

    def test_active_without_both_real_nonempty_pcm_receipts_is_not_warmed(self):
        state = active_state()
        receipt = warm_receipt(state, "cosy")
        self.assertFalse(receipt["asr"]["spoken_wav_transcription_tested"])
        self.assertFalse(receipt["tts"]["speaker_output_measured"])
        bad = []
        for field in ("ready", "warmup_completed", "active"):
            value = copy.deepcopy(state)
            value["local_asr"][field] = False
            bad.append(value)
        for samples in (0, True, None):
            value = copy.deepcopy(state)
            value["local_speech"]["receipt"]["languages"][0]["samples"] = samples
            bad.append(value)
        value = copy.deepcopy(state)
        value["local_speech"]["receipt"]["languages"][1]["language"] = "zh"
        bad.append(value)
        value = copy.deepcopy(state)
        value["local_speech"]["receipt"]["languages"][0]["transport"]["samples"] = 479
        bad.append(value)
        for value in bad:
            with self.assertRaises(SmokeFailure):
                warm_receipt(value, "cosy")

    def test_allowlist_excludes_capture_playback_target_and_model_access(self):
        for path in ("/api/audio/record/start", "/api/audio/speak", "/api/audio/devices",
                     "/api/camera/select", "/api/ptz/start", "/v1/audio/speech"):
            self.assertNotIn(("POST", path), ALLOWED)
            self.assertNotIn(("GET", path), ALLOWED)

    def test_stopped_label_without_bound_linux_cleanup_is_not_success(self):
        instance = "a" * 32
        stopped = {"phase": "stopped", "active": False, "busy": False,
            "linux_cleanup_confirmed": True, "client_deactivation_confirmed": True, "instance_id": instance,
            "ownership_stop": {"linux_cleanup_confirmed": True, "bridge_exit_code": 0, "normal_owner_stop": True,
                               "receipt": {"state": "stopped", "instance_id": instance}}}
        self.assertEqual(confirm_tts_stop(stopped), instance)
        for key, value in (("client_deactivation_confirmed", False), ("active", True),
                           ("phase", "cleanup_failed"), ("instance_id", "b" * 32)):
            bad = copy.deepcopy(stopped)
            bad[key] = value
            with self.assertRaises(SmokeFailure):
                confirm_tts_stop(bad)

    def test_video_summary_keeps_preparation_separate_and_counts_valid_age_denominator(self):
        with tempfile.TemporaryDirectory() as tmp:
            folder = Path(tmp)
            row = {"phase": "preparation", "elapsed_ms": 0, "frame_id": 1, "delivery_id": 1,
                   "width": 1280, "height": 720, "capture_ids_skipped": 0, "capture_age_clock_valid": True,
                   "preview_jpeg_reused": True, "confirmed_track_count": 0, "capture_return_to_receive_ms": 10}
            rows = [row, {**row, "phase": "coexistence", "elapsed_ms": 1000, "frame_id": 2, "delivery_id": 2},
                    {**row, "phase": "coexistence", "elapsed_ms": 1100, "frame_id": 4, "delivery_id": 3,
                     "capture_ids_skipped": 1, "capture_age_clock_valid": False}]
            del rows[-1]["capture_return_to_receive_ms"]
            (folder / "video-frames.jsonl").write_text(''.join(json.dumps(r)+'\n' for r in rows), encoding="utf-8")
            preview = Preview(None, folder, None, None, {})
            summary = preview.summarize()
            self.assertEqual(summary["phases"]["preparation"]["frames"], 1)
            measured = summary["phases"]["coexistence"]
            self.assertEqual(measured["received_unique_fps"], 10)
            self.assertEqual(measured["capture_ids_skipped"], 1)
            self.assertEqual(measured["clock_invalid_frames"], 1)
            self.assertEqual(measured["distributions"]["capture_return_to_receive_ms"]["count"], 1)


if __name__ == "__main__":
    unittest.main()
