"""Analyze network PCM availability. This is not a speaker or listening test."""
import argparse
from array import array
import json
import math
from pathlib import Path
import wave

parser = argparse.ArgumentParser()
parser.add_argument("report", type=Path)
args = parser.parse_args()
report = json.loads(args.report.read_text(encoding="utf-8"))
rows = []
for row in report["rows"]:
    if not row["success"]:
        continue
    consumed_bytes, earliest_continuous_start, prefill_start = 0, 0.0, None
    for chunk in row["chunks"]:
        earliest_continuous_start = max(earliest_continuous_start, chunk["at_ms"] - consumed_bytes / 48)
        consumed_bytes += chunk["bytes"]
        if prefill_start is None and consumed_bytes / 48 >= 240:
            prefill_start = chunk["at_ms"]
    with wave.open(str(args.report.parent / row["wav"]), "rb") as wav:
        assert wav.getframerate() == 24000 and wav.getnchannels() == 1 and wav.getsampwidth() == 2
        samples = array("h", wav.readframes(wav.getnframes()))
    # Three consecutive 20 ms RMS windows above -46 dBFS. A signal heuristic,
    # not proof of a phoneme, intelligibility, hearing threshold or device output.
    frame = 480
    rms = [math.sqrt(sum((v / 32768) ** 2 for v in samples[i:i + frame]) / len(samples[i:i + frame]))
           for i in range(0, len(samples), frame)]
    onset = next((i * 20 for i in range(len(rms) - 2) if min(rms[i:i + 3]) >= .005), None)
    rows.append({"id": row["id"], "first_pcm_bytes_ms": row["first_pcm_bytes_ms"],
                 "continuous_start_lower_bound_ms": earliest_continuous_start,
                 "first_240ms_available_at_ms": prefill_start,
                 "additional_stall_if_240ms_prefill_ms": max(0.0, earliest_continuous_start - prefill_start) if prefill_start else None,
                 "signal_onset_in_wav_ms": onset,
                 "signal_onset_lower_bound_from_request_ms": earliest_continuous_start + onset if onset is not None else None})
output = {"type": "network_schedule_and_signal_heuristic", "speaker_output": False,
          "assumptions": "PCM chunks available at measured read times; ideal output clock; mono 24k PCM16; 240ms prefill. RMS onset is not a listening score.",
          "rows": rows}
path = args.report.parent / "buffer-analysis.json"
path.write_text(json.dumps(output, indent=2), encoding="utf-8")
print(json.dumps(output))
