"""Stdlib-only protocol and fake-engine checks; never loads a model."""
import ast
import builtins
import copy
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock, patch

import embedding_worker as worker


INSTANCE = "0123456789abcdef0123456789abcdef"


def command(op="encode", text="A private input", request="request-1", role="query"):
    value = {"protocol_version": 1, "instance_id": INSTANCE, "op": op}
    if op in ("start", "encode"):
        value["profile"] = copy.deepcopy(worker.PROFILE)
    if op == "encode":
        value.update(request_id=request, role=role, text=text,
                     input_sha256=hashlib.sha256(text.encode("utf-8")).hexdigest())
    return value


class FakeEncoder:
    def __init__(self):
        self.ready_info = {"warmup_tokens": 7, "timings": {
            "hash_audit_ms": 0.1, "model_load_ms": 0.2, "warmup_ms": 0.3, "total_ms": 0.6}}
        self.calls = []

    def encode(self, text):
        self.calls.append(text)
        return {"tokens": 7, "vector": [1.0] + [0.0] * 1023,
                "timings": {"tokenize_ms": 0.1, "inference_ms": 0.2, "total_ms": 0.3}}


def execute(messages, engine=None, factory=None):
    engine = engine or FakeEncoder()
    factory = factory or Mock(return_value=engine)
    output = io.BytesIO()
    source = io.BytesIO(b"".join(worker.wire_bytes(message) for message in messages))
    status = worker.run("/synthetic/offline-model", INSTANCE, source, output, engine_factory=factory)
    return status, [worker.decode_line(line) for line in output.getvalue().splitlines(keepends=True)], factory, output.getvalue()


class WorkerProtocolTests(unittest.TestCase):
    def test_module_import_is_stdlib_only_and_assets_match_the_frozen_downloader(self):
        original_import = builtins.__import__
        def restricted(name, *args, **kwargs):
            if name.split(".")[0] in {"torch", "transformers", "numpy", "download_bge_m3"}:
                self.fail("Heavy or downloader import during module import")
            return original_import(name, *args, **kwargs)
        spec = importlib.util.spec_from_file_location("embedding_worker_import_check", Path(worker.__file__))
        module = importlib.util.module_from_spec(spec)
        with patch("builtins.__import__", side_effect=restricted):
            spec.loader.exec_module(module)
        source = Path(worker.__file__).parents[1] / "memory-lab/download_bge_m3.py"
        tree = ast.parse(source.read_text(encoding="utf-8"))
        assets = next(ast.literal_eval(node.value) for node in tree.body
                      if isinstance(node, ast.Assign) and any(isinstance(t, ast.Name) and t.id == "ASSETS" for t in node.targets))
        self.assertEqual(worker.ASSETS, assets)

    def test_profile_is_the_complete_rust_registry_identity(self):
        profile = worker.PROFILE
        self.assertEqual(set(profile), {"model", "revision", "model_key", "weight_sha256", "tokenizer_sha256",
            "pooling", "query_prefix", "document_prefix", "dimension", "artifact", "normalization", "maximum_tokens"})
        parts = [profile[k] for k in ["revision", "weight_sha256", "tokenizer_sha256", "query_prefix", "document_prefix", "pooling", "dimension"]]
        self.assertEqual(profile["model_key"], hashlib.sha256(json.dumps(parts, separators=(",", ":")).encode()).hexdigest())
        self.assertEqual((profile["dimension"], profile["maximum_tokens"], profile["artifact"]), (1024, 512, "pytorch-bin-native-fp32"))

    def test_wire_is_bounded_utf8_object_json_with_one_lf_and_finite_values(self):
        self.assertEqual(worker.decode_line(worker.wire_bytes({"text": "中文\nunchanged"})), {"text": "中文\nunchanged"})
        empty_size = len(worker.wire_bytes({"padding": ""}))
        exact = worker.wire_bytes({"padding": "x" * (worker.MAX_LINE - empty_size)})
        self.assertEqual(len(exact), worker.MAX_LINE)
        with self.assertRaises(worker.ProtocolError):
            worker.wire_bytes({"padding": "x" * (worker.MAX_LINE - empty_size + 1)})
        for raw in [b'{}', b'[]\n', b'{}\n{}\n', b'{"n":NaN}\n', b'{"n":Infinity}\n',
                    b'{"n":1e999}\n', b'{"n":1,"n":2}\n', b'{"s":"\xff"}\n', b'{"s":"\\ud800"}\n']:
            with self.subTest(raw=raw), self.assertRaises(worker.ProtocolError):
                worker.decode_line(raw)
        with self.assertRaises(worker.ProtocolError):
            worker.wire_bytes({"n": float("nan")})

    def test_commands_bind_exact_fields_instance_profile_and_original_text_hash(self):
        for op in ["start", "encode", "stop"]:
            self.assertEqual(worker.validate_command(command(op), INSTANCE), op)
        changes = [lambda c: c.update(instance_id="f"*32), lambda c: c.update(extra=True),
                   lambda c: c.update(op={}), lambda c: c.update(protocol_version=True),
                   lambda c: c.update(input_sha256="0"*64), lambda c: c.update(request_id="bad id"),
                   lambda c: c.update(role="other"), lambda c: c["profile"].update(dimension=1024.0),
                   lambda c: c["profile"].update(query_prefix="query: ")]
        for change in changes:
            item = command(text="  原文\nunchanged  ")
            change(item)
            with self.assertRaises(worker.ProtocolError):
                worker.validate_command(item, INSTANCE)

    def test_environment_is_offline_cpu_only_and_does_not_mutate_the_owner(self):
        inherited = {"PATH": "kept", "CUDA_VISIBLE_DEVICES": "0", "HF_TOKEN": "private", "https_proxy": "private"}
        expected = dict(inherited)
        environment = worker.cpu_environment(inherited)
        self.assertEqual(inherited, expected)
        self.assertEqual(environment["CUDA_VISIBLE_DEVICES"], "")
        self.assertEqual(environment["HF_HUB_OFFLINE"], "1")
        self.assertEqual(environment["TRANSFORMERS_OFFLINE"], "1")
        self.assertEqual(environment["OMP_NUM_THREADS"], "2")
        self.assertEqual(environment["PATH"], "kept")
        self.assertNotIn("HF_TOKEN", environment)
        self.assertNotIn("https_proxy", environment)

    def test_git_blob_and_lfs_asset_checks_reject_different_bytes(self):
        data = b"pinned test bytes"
        git = hashlib.sha1(b"blob " + str(len(data)).encode() + b"\0" + data).hexdigest()
        sha = hashlib.sha256(data).hexdigest()
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "asset"
            path.write_bytes(data)
            worker._verify_asset(path, ("asset", len(data), git, None))
            worker._verify_asset(path, ("asset", len(data), "pointer-oid-not-file-hash", sha))
            path.write_bytes(b"x" * len(data))
            for asset in [("asset", len(data), git, None), ("asset", len(data), "pointer", sha)]:
                with self.assertRaises(worker.StartupError):
                    worker._verify_asset(path, asset)

    def test_tokenization_never_truncates_and_rejects_513_before_inference(self):
        tokenizer = Mock(return_value={"input_ids": [0]*512, "attention_mask": [1]*512})
        tokens, _, _ = worker._tokenize(tokenizer, "Original text")
        self.assertEqual(tokens, 512)
        tokenizer.assert_called_once_with("Original text", add_special_tokens=True, padding=False, truncation=False,
                                         return_attention_mask=True, return_token_type_ids=False)
        tokenizer.return_value = {"input_ids": [0]*513, "attention_mask": [1]*513}
        with self.assertRaises(worker.InputTooLong) as caught:
            worker._tokenize(tokenizer, "Original text")
        self.assertEqual(caught.exception.tokens, 513)

    def test_one_load_two_encodes_and_normal_stop_preserve_only_request_identity(self):
        engine = FakeEncoder()
        first = command(text="private source text", request="a", role="document")
        second = command(text="private query text", request="b", role="query")
        status, rows, factory, raw = execute([first, second, command("stop")], engine)
        self.assertEqual(status, 0)
        factory.assert_called_once_with("/synthetic/offline-model")
        self.assertEqual(engine.calls, [first["text"], second["text"]])
        self.assertEqual([row["state"] for row in rows], ["loading", "ready", "encoded", "encoded", "worker_stopped"])
        self.assertEqual(set(rows[0]), {"protocol_version", "instance_id", "state", "profile", "ready", "warmup_completed"})
        self.assertEqual(set(rows[1]["timings"]), {"hash_audit_ms", "model_load_ms", "warmup_ms", "total_ms"})
        self.assertEqual(rows[1]["load_count"], 1)
        for row, request in zip(rows[2:4], [first, second]):
            self.assertEqual(set(row), {"protocol_version", "instance_id", "state", "request_id", "role", "input_sha256", "text_sha256", "profile", "tokens", "vector", "timings"})
            self.assertEqual(row["input_sha256"], request["input_sha256"])
            self.assertEqual(row["text_sha256"], request["input_sha256"])
            self.assertEqual(row["role"], request["role"])
            self.assertEqual(len(row["vector"]), 1024)
        self.assertNotIn(b"private source text", raw)
        self.assertNotIn(b"private query text", raw)
        self.assertEqual(rows[-1], {"protocol_version": 1, "instance_id": INSTANCE,
                                   "state": "worker_stopped", "ready": False, "warmup_completed": False})

    def test_error_messages_are_fixed_and_overlong_tokens_are_actual(self):
        engine = FakeEncoder()
        engine.encode = Mock(side_effect=[worker.InputTooLong(513), ValueError("secret input or path")])
        status, rows, _, raw = execute([command(request="long"), command(request="failed"), command("stop")], engine)
        self.assertEqual(status, 0)
        self.assertEqual(engine.encode.call_count, 2)
        for row in rows[2:4]:
            self.assertEqual(set(row), {"protocol_version", "instance_id", "state", "request_id", "role", "input_sha256", "text_sha256", "profile", "code", "tokens"})
        self.assertEqual((rows[2]["code"], rows[2]["tokens"]), ("input_too_long", 513))
        self.assertEqual((rows[3]["code"], rows[3]["tokens"]), ("encode_failed", None))
        self.assertNotIn(b"secret input or path", raw)

    def test_invalid_owner_command_or_second_start_terminates_without_encoding(self):
        for item in [command("start"), {**command(), "extra": "secret"}]:
            engine = FakeEncoder()
            status, rows, _, raw = execute([item], engine)
            self.assertEqual(status, 2)
            self.assertEqual(rows[-1]["code"], "invalid_owner_protocol")
            self.assertEqual(engine.calls, [])
            self.assertNotIn(b"secret", raw)
        status, rows, _, _ = execute([])
        self.assertEqual(status, 0)
        self.assertEqual(rows[-1]["state"], "worker_stopped")

    def test_encode_failure_count_is_null_or_an_in_range_actual_count(self):
        engine = FakeEncoder()
        engine.encode = Mock(side_effect=[worker.EncodeFailure(0), worker.EncodeFailure(513), worker.EncodeFailure(512)])
        status, rows, _, _ = execute([command(request="zero"), command(request="oversize"), command(request="actual"), command("stop")], engine)
        self.assertEqual(status, 0)
        self.assertEqual([row["tokens"] for row in rows[2:5]], [None, None, 512])
        self.assertTrue(all(row["code"] == "encode_failed" for row in rows[2:5]))

    def test_startup_failure_has_no_readiness_or_private_exception(self):
        for code in ["asset_mismatch", "local_model_load_failed", "warmup_failed"]:
            factory = Mock(side_effect=worker.StartupError(code))
            status, rows, _, _ = execute([], factory=factory)
            self.assertEqual(status, 1)
            self.assertEqual([row["state"] for row in rows], ["loading", "failed"])
            self.assertEqual(rows[-1]["code"], code)

    def test_invalid_vector_or_timing_is_an_encode_failure_not_a_false_success(self):
        for field, replacement in [("vector", [0.0]*1024), ("vector", [float("nan")]+[0.0]*1023),
                                   ("vector", [1.0]+[0.0]*383), ("timings", {"tokenize_ms": -1, "inference_ms": 0, "total_ms": 0})]:
            engine = FakeEncoder()
            value = engine.encode("fixture")
            value[field] = replacement
            engine.encode = Mock(return_value=value)
            status, rows, _, _ = execute([command(), command("stop")], engine)
            self.assertEqual(status, 0)
            self.assertEqual(rows[2]["state"], "encode_error")
            self.assertEqual(rows[2]["code"], "encode_failed")
            self.assertNotIn("vector", rows[2])


if __name__ == "__main__":
    unittest.main()
