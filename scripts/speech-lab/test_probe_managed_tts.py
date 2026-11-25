"""Offline contract/credential tests. No WSL, model, network or audio devices."""
import json
from pathlib import Path
import queue
import tempfile
import time
import unittest
from unittest import mock

import probe_managed_tts as probe


INSTANCE = "1" * 32


def starting():
    return {"protocol_version": 1, "instance_id": INSTANCE, "state": "starting",
            "engine": "cosy", "model": "tutor-cosy-tts",
            "endpoint": "http://127.0.0.1:18869/v1/audio/speech", "ready": False,
            "warm": False, "pid": 123, "process_group": 123}


def prepared():
    return {"test_type": "rust_local_tts_prepare_bilingual_synthetic", "engine": "cosy",
            "success": True, "outcome": "completed",
            "status": {"phase": "warm_ready_pending_install"},
            "receipt": {"languages": [{"language": "zh", "samples": 1200},
                                       {"language": "en", "samples": 1200}]},
            "client_installed": False, "speaker_output": False, "microphone_opened": False,
            "played": False, "private_fixture_cleaned": True}


class ContractTests(unittest.TestCase):
    def test_stderr_redacts_a_credential_split_between_pipe_chunks(self):
        token = "synthetic_private_credential_1234567890"
        stream, events = mock.Mock(), queue.Queue()
        stream.read1.side_effect = [b"before " + token[:15].encode(),
                                   token[15:].encode() + b" after\n", b""]
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "stderr.log"
            probe.stderr_reader(stream, path, token, events)
            text = path.read_text(encoding="utf-8")
            self.assertEqual(text, "before [REDACTED] after\n")
            self.assertTrue(events.empty())

    def test_stderr_keeps_draining_after_log_open_failure(self):
        stream, events, path = mock.Mock(), queue.Queue(), mock.Mock()
        path.open.side_effect = OSError("synthetic disk failure")
        stream.read1.side_effect = [b"log one", b"log two", b""]
        probe.stderr_reader(stream, path, "synthetic-token", events)
        self.assertEqual(stream.read1.call_count, 3)
        self.assertEqual(events.get_nowait(), ("error", "wrapper_stderr_log_failed"))

    def test_foreign_owner_endpoint_and_claimed_warm_start_are_rejected(self):
        accepted = probe.wrapper_event(json.dumps(starting()), INSTANCE, "cosy")
        self.assertFalse(accepted["warm"])
        self.assertNotIn("pid", accepted)  # Receipt PIDs never become Windows kill targets.
        for field, value in (("instance_id", "2" * 32), ("protocol_version", 2),
                             ("endpoint", "http://example.invalid/v1/audio/speech"),
                             ("model", "other"), ("ready", True), ("warm", True)):
            event = starting()
            event[field] = value
            with self.subTest(field=field), self.assertRaises(probe.ProbeFailure):
                probe.wrapper_event(json.dumps(event), INSTANCE, "cosy")

    def test_preparation_requires_both_languages_and_no_install_or_playback(self):
        self.assertEqual(probe.preparation_receipt(json.dumps(prepared()), "cosy"), prepared())
        for field, value in (("success", False), ("client_installed", True),
                             ("played", True), ("microphone_opened", True),
                             ("status", None), ("private_fixture_cleaned", False)):
            item = prepared()
            item[field] = value
            with self.subTest(field=field), self.assertRaises(probe.ProbeFailure):
                probe.preparation_receipt(json.dumps(item), "cosy")
        for languages in ([{"language": "zh", "samples": 1200}], [None, None],
                          [{"language": "zh", "samples": 1200}, {"language": "en", "samples": 0}]):
            item = prepared()
            item["receipt"]["languages"] = languages
            with self.assertRaises(probe.ProbeFailure):
                probe.preparation_receipt(json.dumps(item), "cosy")

    def test_credential_cleanup_removes_exact_owned_secrets_and_preserves_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            fixture = Path(directory) / "fixture"
            key = fixture / probe.ENGINES["qwen"][2]
            key.parent.mkdir(parents=True)
            key.write_text("synthetic-token", encoding="utf-8")
            child = fixture / "runtime/tts-probe-owned/runtime/tts-service.json"
            child.parent.mkdir(parents=True)
            child.write_text(json.dumps({"bearer_token": "synthetic-token"}), encoding="utf-8")
            evidence = child.with_name("local-speech-policy.json")
            evidence.write_text("kept", encoding="utf-8")
            self.assertEqual(probe.cleanup_credentials(fixture, "qwen", "synthetic-token"), 2)
            self.assertFalse(key.exists())
            self.assertFalse(child.exists())
            self.assertEqual(evidence.read_text(encoding="utf-8"), "kept")
            self.assertTrue(child.parent.is_dir())

    def test_changed_credential_is_not_deleted_or_reported_as_clean(self):
        with tempfile.TemporaryDirectory() as directory:
            fixture = Path(directory)
            key = fixture / probe.ENGINES["qwen"][2]
            key.parent.mkdir(parents=True)
            key.write_text("different-owner-token", encoding="utf-8")
            with self.assertRaises(probe.ProbeFailure):
                probe.cleanup_credentials(fixture, "qwen", "synthetic-token")
            self.assertTrue(key.exists())

    def test_bridge_exit_alone_and_cleanup_failure_never_prove_linux_shutdown(self):
        for terminal in (None, "cleanup_failed", "stopped"):
            events = queue.Queue()
            report = {"wrapper_events": [], "errors": [], "bridge_terminated_only": False}
            if terminal:
                item = {"protocol_version": 1, "instance_id": INSTANCE, "state": terminal}
                if terminal == "stopped":
                    item.update(reason="owner_stop", service_exit_code=-15, forced_group_stop=False)
                events.put(("event", probe.wrapper_event(json.dumps(item), INSTANCE, "cosy")))
            process = mock.Mock(returncode=0)
            process.poll.return_value = 0
            probe.stop_wrapper(process, events, [], INSTANCE, report, time.monotonic())
            self.assertEqual(report["linux_cleanup_confirmed"], terminal == "stopped")
            self.assertEqual(not report["errors"], terminal == "stopped")
            process.terminate.assert_not_called()

    def test_unexpected_service_exit_can_confirm_cleanup_but_never_probe_success(self):
        events = queue.Queue()
        events.put(("event", {"state": "stopped", "reason": "service_exited"}))
        report = {"wrapper_events": [], "errors": [], "bridge_terminated_only": False}
        process = mock.Mock(returncode=1)
        process.poll.return_value = 1
        probe.stop_wrapper(process, events, [], INSTANCE, report, time.monotonic())
        self.assertTrue(report["linux_cleanup_confirmed"])
        self.assertIn("wrapper_did_not_finish_normal_owner_shutdown", report["errors"])

    def test_preflight_failure_proof_is_preserved_but_never_becomes_a_successful_run(self):
        proof = {"protocol_version": 1, "instance_id": INSTANCE, "state": "failed",
                 "ready": False, "warm": False, "linux_cleanup_confirmed": True,
                 "process_started": False, "error_type": "RuntimeError", "untrusted_detail": "not retained"}
        for field in (None, "ready", "warm", "linux_cleanup_confirmed", "process_started"):
            raw = proof.copy()
            if field is not None:
                raw.pop(field)
            event = probe.wrapper_event(json.dumps(raw), INSTANCE, "qwen")
            self.assertNotIn("untrusted_detail", event)
            self.assertEqual(event["error_type"], "RuntimeError")
            events = queue.Queue()
            events.put(("event", event))
            report = {"wrapper_events": [], "errors": [], "bridge_terminated_only": False}
            process = mock.Mock(returncode=1)
            process.poll.return_value = 1
            probe.stop_wrapper(process, events, [], INSTANCE, report, time.monotonic())
            self.assertEqual(report["linux_cleanup_confirmed"], field is None)
            self.assertTrue(report["errors"])
            process.terminate.assert_not_called()

    def test_bridge_timeout_cannot_turn_a_late_stopped_event_into_success(self):
        events = queue.Queue()
        events.put(("event", {"state": "stopped", "reason": "owner_stop"}))
        report = {"wrapper_events": [], "errors": [], "bridge_terminated_only": False}
        process = mock.Mock(returncode=None)
        process.poll.return_value = None

        def terminated():
            process.returncode = 1
            process.poll.return_value = 1

        process.terminate.side_effect = terminated
        with mock.patch.object(probe, "STOP_SECONDS", 0):
            probe.stop_wrapper(process, events, [], INSTANCE, report, time.monotonic())
        process.terminate.assert_called_once_with()
        process.stdin.close.assert_called_once_with()
        sent = json.loads(process.stdin.write.call_args.args[0])
        self.assertEqual(sent, {"op": "stop", "instance_id": INSTANCE})
        self.assertTrue(report["bridge_terminated_only"])
        self.assertFalse(report["linux_cleanup_confirmed"])
        self.assertFalse(report["stopped"])


if __name__ == "__main__":
    unittest.main()
