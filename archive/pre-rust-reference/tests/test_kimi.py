import asyncio
import json
import unittest
from pathlib import Path
import tempfile
from ai_tutor.kimi import KimiError, KimiProvider, MAX_OUTPUT_BYTES, parse_stream_json, _dump_table, build_prompt, validate_windows_command_budget
try:
    import tomllib
except ImportError:
    import tomli as tomllib


def envelope(*messages):
    return b"\n".join(json.dumps(item, ensure_ascii=False).encode("utf-8") for item in messages)


class ProtocolTests(unittest.TestCase):
    def test_only_assistant_text_survives_version_and_resume_hint(self):
        data = envelope({"role": "meta", "type": "system.version", "version": "2.3.1"},
                        {"role": "assistant", "content": "中文主控与测距"},
                        {"role": "meta", "type": "session.resume_hint", "session_id": "test",
                         "command": "dangerous shell string never executed", "content": "not shown"})
        self.assertEqual(parse_stream_json(data), ("中文主控与测距", None))

    def test_tool_activity_rejected_even_after_valid_reply(self):
        for attack in ({"role": "tool", "content": "secret"},
                       {"role": "assistant", "tool_calls": [{"name": "Read"}], "content": ""},
                       {"role": "assistant", "content": [{"type": "tool_use", "name": "Bash"}]}):
            with self.assertRaises(KimiError):
                parse_stream_json(envelope({"role": "assistant", "content": "valid"}, attack))

    def test_unknown_protocol_and_invalid_encoding_fail_closed(self):
        for payload in (b"not JSON\n", b"\xff", b"[]\n",
                        envelope({"role": "meta", "type": "new-event", "content": "do not expose"}),
                        envelope({"role": "meta", "type": "system.version", "version": "9.0.0"})):
            with self.assertRaises(KimiError):
                parse_stream_json(payload)

    def test_thinking_blocks_not_returned(self):
        data = envelope({"role": "assistant", "content": [{"type": "thinking", "thinking": "hidden"},
                        {"type": "text", "text": "public"}]})
        self.assertEqual(parse_stream_json(data)[0], "public")

    def test_config_serialization_preserves_provider_reference(self):
        config = {"default_model": "test", "providers": {"managed:kimi-code": {
            "type": "kimi", "oauth": {"storage": "file", "key": "identifier-not-a-token"}}},
            "telemetry": False, "models": {"test": {"capabilities": ["thinking", "tool_use"]}}}
        self.assertEqual(tomllib.loads(_dump_table(config)), config)

    def test_output_memory_is_bounded(self):
        async def run():
            stream = asyncio.StreamReader()
            stream.feed_data(b"x" * 9000)
            stream.feed_eof()
            provider = object.__new__(KimiProvider)
            with self.assertRaises(KimiError):
                await provider._read_bounded(stream, 8000)
        asyncio.run(run())

    def test_schema_is_sent_once_without_mutating_provenance_request(self):
        schema = {"marker": "ONLY_ONE_SCHEMA"}
        request = {"request_id": "test", "context": {"reply_schema": schema, "user_input": "停车"}, "_private": "never send"}
        prompt = build_prompt("policy", schema, request)
        self.assertEqual(prompt.count("ONLY_ONE_SCHEMA"), 1)
        self.assertNotIn("never send", prompt)
        self.assertEqual(request["context"]["reply_schema"], schema)
        self.assertIn("停车", prompt)

    def test_windows_command_budget_counts_surrogate_pairs_and_quoting(self):
        validate_windows_command_budget(["C:\\some path\\kimi.exe", "-p", "正常中文"])
        with self.assertRaises(KimiError):
            validate_windows_command_budget(["kimi.exe", "-p", "😀" * 16000])
        with self.assertRaises(KimiError):
            validate_windows_command_budget(["kimi.exe", "-p", '"' * 16000])

    def test_cancel_unknown_and_queued_requests_leaves_no_unbounded_markers(self):
        async def run(root):
            provider = KimiProvider(root)
            for i in range(100):
                await provider.cancel("not-started-" + str(i))
            self.assertEqual(provider._cancelled, set())
            provider._ready = True
            await provider._lock.acquire()
            task = asyncio.create_task(provider.generate({"request_id": "queued", "learner_id": "A"}))
            await asyncio.sleep(0)
            await provider.cancel("queued")
            with self.assertRaises(asyncio.CancelledError):
                await task
            provider._lock.release()
            self.assertEqual(provider._cancelled, set())
            self.assertEqual(provider._calls, {})
            self.assertEqual(provider._owners, {})
        with tempfile.TemporaryDirectory() as root:
            asyncio.run(run(Path(root)))


if __name__ == "__main__":
    unittest.main()
