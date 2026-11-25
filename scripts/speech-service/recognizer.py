"""CPU streaming adapters. No microphone, playback, or transcript persistence."""
from pathlib import Path


class _StreamingRecognizer:
    def warm(self):
        """Synthetic silence only; never consume a live recognition stream."""
        stream = self.create()
        stream.accept_waveform(16000, self.np.zeros(16000, dtype=self.np.float32))
        self._decode(stream)

    def create(self):
        return self.model.create_stream()

    def _decode(self, stream):
        while self.model.is_ready(stream):
            self.model.decode_stream(stream)
        return self.model.get_result(stream)

    def chunk(self, stream, pcm: bytes):
        samples = self.np.frombuffer(pcm, dtype="<i2").astype(self.np.float32) / 32768.0
        stream.accept_waveform(16000, samples)
        return self._decode(stream)


class Zipformer(_StreamingRecognizer):
    provider = "sherpa-onnx / bilingual Zipformer int8 / CPU"

    def __init__(self, model: Path, threads: int = 2, warmup: bool = True):
        import numpy as np
        import sherpa_onnx

        self.np = np
        self.runtime_version = sherpa_onnx.__version__
        self.model = sherpa_onnx.OnlineRecognizer.from_transducer(
            tokens=str(model / "tokens.txt"),
            encoder=str(model / "encoder-epoch-99-avg-1.int8.onnx"),
            decoder=str(model / "decoder-epoch-99-avg-1.onnx"),
            joiner=str(model / "joiner-epoch-99-avg-1.int8.onnx"),
            num_threads=threads, sample_rate=16000, enable_endpoint_detection=False,
            decoding_method="greedy_search", provider="cpu")
        if warmup:
            self.warm()

    def finish(self, stream):
        # Padding flushes the model's right context; it is not recorded audio.
        stream.accept_waveform(16000, self.np.zeros(9600, dtype=self.np.float32))
        stream.input_finished()
        return self._decode(stream)


class Paraformer(_StreamingRecognizer):
    """Explicit historical ONNX diagnostic candidate; never the default backend."""
    provider = "sherpa-onnx / bilingual Paraformer int8 8e40c43 / CPU"

    def __init__(self, model: Path, threads: int = 2, warmup: bool = True):
        import numpy as np
        import sherpa_onnx

        self.np = np
        self.runtime_version = sherpa_onnx.__version__
        self.model = sherpa_onnx.OnlineRecognizer.from_paraformer(
            tokens=str(model / "tokens.txt"),
            encoder=str(model / "encoder.int8.onnx"),
            decoder=str(model / "decoder.int8.onnx"),
            num_threads=threads, sample_rate=16000, feature_dim=80,
            enable_endpoint_detection=False, decoding_method="greedy_search", provider="cpu")
        if warmup:
            self.warm()

    def finish(self, stream):
        # Fixed 660 ms model right context, supplied immediately, never source
        # padding or an endpoint wait. Keep Zipformer's 600 ms path unchanged.
        stream.accept_waveform(16000, self.np.zeros(10560, dtype=self.np.float32))
        stream.input_finished()
        # sherpa-onnx 2.0.1 Paraformer supports the final short feature chunk
        # through this option; input_finished() alone does not enable it.
        stream.set_option("is_final", "1")
        return self._decode(stream)
