"""No native imports, models or devices: constructor, PCM and owner regressions."""
import io
import json
from pathlib import Path
import struct
import subprocess
import sys
import threading
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import managed_asr
import recognizer
import test_managed_asr as owner_support


class Array(list):
    def astype(self, dtype):
        assert dtype == "float32"
        return self

    def __truediv__(self, divisor):
        return Array(value / divisor for value in self)


class Stream:
    def __init__(self):
        self.events = []
        self.ready = False

    def accept_waveform(self, rate, samples):
        self.events.append(("pcm", rate, list(samples)))
        self.ready = True

    def input_finished(self):
        self.events.append(("input_finished",))

    def set_option(self, key, value):
        self.events.append(("option", key, value))


class Model:
    def __init__(self):
        self.streams = []
        self.result = ""

    def create_stream(self):
        stream = Stream()
        self.streams.append(stream)
        return stream

    def is_ready(self, stream):
        return stream.ready

    def decode_stream(self, stream):
        stream.events.append(("decode",))
        stream.ready = False

    def get_result(self, stream):
        return self.result


class AdapterTests(unittest.TestCase):
    def setUp(self):
        self.constructed = []

        def create(kind, kwargs):
            model = Model()
            self.constructed.append((kind, kwargs, model))
            return model

        numpy = SimpleNamespace(
            float32="float32",
            zeros=lambda count, dtype: Array([0.0] * count),
            frombuffer=lambda data, dtype: Array(struct.unpack("<" + "h" * (len(data) // 2), data)))
        sherpa = SimpleNamespace(
            __version__="fake-no-native-runtime",
            OnlineRecognizer=SimpleNamespace(
                from_transducer=lambda **kwargs: create("zipformer", kwargs),
                from_paraformer=lambda **kwargs: create("paraformer", kwargs)))
        native = patch.dict(sys.modules, {"numpy": numpy, "sherpa_onnx": sherpa})
        native.start()
        self.addCleanup(native.stop)

    def test_allowlisted_factory_keeps_zipformer_default_and_selects_exact_candidate(self):
        model_path = Path("synthetic-assets")
        default = managed_asr.load_engine(model_path)
        candidate = managed_asr.load_engine(model_path, "paraformer")
        self.assertIsInstance(default, recognizer.Zipformer)
        self.assertIsInstance(candidate, recognizer.Paraformer)
        self.assertEqual([row[0] for row in self.constructed], ["zipformer", "paraformer"])
        for kind, kwargs, model in self.constructed:
            self.assertEqual(model.streams, [])  # Lifecycle owns warmup after its gate.
            self.assertEqual(kwargs["num_threads"], 2)
            self.assertEqual(kwargs["sample_rate"], 16000)
            self.assertEqual(kwargs["provider"], "cpu")
            self.assertFalse(kwargs["enable_endpoint_detection"])
            self.assertEqual(kwargs["decoding_method"], "greedy_search")
            if kind == "paraformer":
                self.assertEqual(kwargs["feature_dim"], 80)
                self.assertEqual(Path(kwargs["encoder"]).name, "encoder.int8.onnx")
                self.assertEqual(Path(kwargs["decoder"]).name, "decoder.int8.onnx")
                self.assertNotIn("joiner", kwargs)
            else:
                self.assertEqual(Path(kwargs["joiner"]).name, "joiner-epoch-99-avg-1.int8.onnx")
        with self.assertRaises(ValueError):
            managed_asr.load_engine(model_path, "unknown")
        self.assertEqual(len(self.constructed), 2)

    def test_pcm_has_no_source_padding_or_filtering_and_empty_is_preserved(self):
        for adapter in (recognizer.Zipformer, recognizer.Paraformer):
            with self.subTest(adapter=adapter.__name__):
                engine = adapter(Path("synthetic-assets"), warmup=False)
                stream = engine.create()
                self.assertEqual(engine.chunk(stream, struct.pack("<hhh", -32768, 0, 16384)), "")
                self.assertEqual(stream.events, [("pcm", 16000, [-1.0, 0.0, 0.5]), ("decode",)])
                engine.model.result = "短 yes"
                self.assertEqual(engine.chunk(stream, struct.pack("<h", 1)), "短 yes")
                self.assertEqual(stream.events[-2], ("pcm", 16000, [1 / 32768.0]))

    def test_final_right_context_and_option_are_backend_specific_and_ordered(self):
        for adapter, count, final_option in (
                (recognizer.Zipformer, 9600, False), (recognizer.Paraformer, 10560, True)):
            with self.subTest(adapter=adapter.__name__):
                engine = adapter(Path("synthetic-assets"), warmup=False)
                stream = engine.create()
                self.assertEqual(engine.finish(stream), "")
                self.assertEqual(stream.events[0], ("pcm", 16000, [0.0] * count))
                expected = [("input_finished",)]
                if final_option:
                    expected.append(("option", "is_final", "1"))
                self.assertEqual(stream.events[1:], expected + [("decode",)])

    def test_warm_stream_is_independent_and_never_becomes_user_audio(self):
        for adapter in (recognizer.Zipformer, recognizer.Paraformer):
            with self.subTest(adapter=adapter.__name__):
                engine = adapter(Path("synthetic-assets"))
                warm_stream = engine.model.streams[0]
                self.assertEqual(warm_stream.events, [("pcm", 16000, [0.0] * 16000), ("decode",)])
                active = engine.create()
                self.assertIsNot(active, warm_stream)
                self.assertEqual(active.events, [])


class ParaformerOwnerTests(owner_support.ManagedTests):
    """Run the existing owner/HTTP invariants against the explicit candidate arm."""
    def start(self, factory=None, timeout=1):
        def load(path):
            self.loaded.append(path)
            return factory() if factory else owner_support.FakeEngine()

        self.thread = threading.Thread(target=lambda: self.exit_code.append(managed_asr.run(
            self.root, self.instance, self.source, self.events, load, timeout,
            backend="paraformer")), daemon=True)
        self.thread.start()
        self.events.next("awaiting_owner")

    def test_backend_receipts_and_model_path_are_explicit(self):
        self.start()
        self.warm()
        self.stop()
        self.assertEqual(self.loaded, [self.root / "models/speech" /
                                      "sherpa-onnx-streaming-paraformer-bilingual-zh-en-8e40c43"])
        self.assertTrue(all(json.loads(line)["backend"] == "paraformer" for line in self.events.all))

    def test_eof_terminates_real_fake_child_without_loading_native_model(self):
        # The copied fixture's own path must not select the production module
        # accidentally: both default and candidate children import this proposal.
        script = self.root / "fake_entry.py"
        paths = [str(Path(managed_asr.__file__).resolve().parent),
                 str(Path(owner_support.__file__).resolve().parent)]
        script.write_text("import sys\nsys.path[:0] = " + repr(paths) + "\n"
                          "from managed_asr import run\n"
                          "from test_managed_asr import FakeEngine\n"
                          "options = {} if sys.argv[3] == 'default' else {'backend':'paraformer'}\n"
                          "raise SystemExit(run(sys.argv[1],sys.argv[2],"
                          "engine_factory=lambda _:FakeEngine(),**options))\n", encoding="utf-8")
        for mode, expected in (("default", "zipformer"), ("candidate", "paraformer")):
            with self.subTest(mode=mode):
                child = subprocess.Popen(
                    [sys.executable, "-B", "-u", str(script), str(self.root), self.instance, mode],
                    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                    text=True, encoding="utf-8")
                try:
                    initial = json.loads(child.stdout.readline())
                    self.assertEqual(initial["state"], "awaiting_owner")
                    self.assertEqual(initial["backend"], expected)
                    child.stdin.write(json.dumps({"protocol_version": 1, "instance_id": self.instance,
                                                  "bearer_token": owner_support.TOKEN}) + "\n")
                    child.stdin.flush()
                    for state in ["loading", "warming", "warm_ready"]:
                        event = json.loads(child.stdout.readline())
                        self.assertEqual((event["state"], event["backend"]), (state, expected))
                    child.stdin.close()
                    terminal = json.loads(child.stdout.readline())
                    self.assertEqual(terminal["state"], "stopped")
                    self.assertTrue(terminal["http_closed"])
                    self.assertEqual(child.wait(timeout=3), 0)
                    self.assertEqual(child.stderr.read(), "")
                finally:
                    if child.poll() is None:
                        child.kill()  # This exact fake child only; never a service lookup.
                        child.wait(timeout=3)
                    if not child.stdin.closed:
                        child.stdin.close()
                    child.stdout.close()
                    child.stderr.close()

    def test_unknown_backend_fails_before_owner_or_http(self):
        output = io.StringIO()
        with patch.object(managed_asr, "ParentControl") as control:
            with patch.object(managed_asr, "ManagedServer") as server:
                with self.assertRaises(ValueError):
                    managed_asr.run(self.root, self.instance, source=io.StringIO(),
                                    output=output, backend="unknown")
                control.assert_not_called()
                server.assert_not_called()
        self.assertEqual(output.getvalue(), "")


if __name__ == "__main__":
    unittest.main()
