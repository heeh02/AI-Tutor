"""Prepare separate 16 kHz ASR copies of generated speech, never edit originals."""
import argparse
import hashlib
import json
from pathlib import Path
import wave
import numpy as np
from scipy.signal import resample_poly

parser = argparse.ArgumentParser()
parser.add_argument("report", type=Path)
parser.add_argument("--output", type=Path, required=True)
args = parser.parse_args()
args.output.mkdir(parents=True, exist_ok=False)
report = json.loads(args.report.read_text(encoding="utf-8"))
corpus = {"version": 1, "source_type": "generated_tts_then_offline_asr",
          "limitations": "Roundtrip errors combine TTS, resampling and ASR. This is not listening, child speech, naturalness or pronunciation scoring.",
          "resampling": "scipy.signal.resample_poly 2/3 default Kaiser window; no trim or denoise", "samples": []}
for row in report["rows"]:
    if not row["success"]:
        continue
    source = args.report.parent / row["wav"]
    with wave.open(str(source), "rb") as wav:
        assert wav.getframerate() == 24000 and wav.getnchannels() == 1 and wav.getsampwidth() == 2
        pcm = np.frombuffer(wav.readframes(wav.getnframes()), dtype="<i2").astype(np.float32) / 32768
    down = np.clip(np.rint(resample_poly(pcm, 2, 3) * 32768), -32768, 32767).astype("<i2")
    target = args.output / source.name
    with wave.open(str(target), "wb") as wav:
        wav.setparams((1, 2, 16000, 0, "NONE", "not compressed"))
        wav.writeframes(down.tobytes())
    corpus["samples"].append({"id": row["id"], "language": row["language"], "text": row["text"],
                              "terms": row["terms"], "wav": str(target.resolve()),
                              "original_sha256": hashlib.sha256(source.read_bytes()).hexdigest()})
(args.output / "corpus.json").write_text(json.dumps(corpus, ensure_ascii=False, indent=2), encoding="utf-8")
print(json.dumps({"samples": len(corpus["samples"]), "original_audio_modified": False}))
