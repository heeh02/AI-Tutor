"""Real local inference over authored fixtures. Never records or plays audio."""
import argparse
import json
import math
from pathlib import Path
import re
import time
import numpy as np
import sherpa_onnx
import soundfile as sf


def units(text, language):
    if language == "en":
        return re.findall(r"[a-z]+|[0-9]+", text.lower())
    return re.findall(r"[\u3400-\u9fff]|[a-z]+|[0-9]+", text.lower())


def distance(reference, actual):
    previous = list(range(len(actual) + 1))
    for i, a in enumerate(reference):
        current = [i + 1]
        for j, b in enumerate(actual):
            current.append(min(current[-1] + 1, previous[j + 1] + 1, previous[j] + (a != b)))
        previous = current
    return previous[-1]


def save(output, report):
    (output / "report.json").write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")


def summarize(rows, keys):
    result = {}
    def group_name(row):
        return "zh-en" if row["id"].startswith("mixed_") else row["language"]
    for language in sorted({group_name(row) for row in rows}):
        group = [row for row in rows if group_name(row) == language]
        result[language] = {}
        for key in keys:
            values = sorted(row[key] for row in group if row.get(key) is not None)
            if values:
                result[language][key] = {"n": len(values), "p50": float(np.median(values)),
                    "p95_nearest_rank": values[math.ceil(len(values) * .95) - 1], "max": values[-1],
                    "small_sample_only": len(values) < 100}
    return result


def tts(args, output):
    corpus = json.loads(args.corpus.read_text(encoding="utf-8-sig"))
    model = args.model.resolve()
    start = time.perf_counter()
    kokoro = sherpa_onnx.OfflineTtsKokoroModelConfig(
        model=str(model / "model.int8.onnx"), voices=str(model / "voices.bin"),
        tokens=str(model / "tokens.txt"), data_dir=str(model / "espeak-ng-data"),
        dict_dir=str(model / "dict"),
        lexicon=",".join(str(model / name) for name in ["lexicon-us-en.txt", "lexicon-zh.txt"]))
    config = sherpa_onnx.OfflineTtsConfig(
        model=sherpa_onnx.OfflineTtsModelConfig(kokoro=kokoro, num_threads=args.threads, provider="cpu"),
        rule_fsts=",".join(str(model / name) for name in ["date-zh.fst", "number-zh.fst", "phone-zh.fst"]),
        max_num_sentences=1)
    engine = sherpa_onnx.OfflineTts(config)
    report = {"test_type": "kokoro_int8_1.1_cpu_synthetic_text", "runtime": sherpa_onnx.__version__,
              "threads": args.threads, "model_load_ms": (time.perf_counter() - start) * 1000,
              "speaker_output": False, "audible_first_sound_measured": False,
              "voices": {"en": {"sid": 0, "name": "af_maple"}, "zh": {"sid": 3, "name": "zf_001"}},
              "callback_granularity": "model sentence callback, not proven acoustic token streaming",
              "rows": []}
    for repetition in range(args.repeats):
        for item in corpus["samples"]:
            chunks = []
            start = time.perf_counter()

            def callback(samples, progress):
                chunks.append({"at_ms": (time.perf_counter() - start) * 1000,
                               "samples": len(samples), "progress": progress})
                return 1

            audio = engine.generate(item["text"], sid=3 if item["language"] == "zh" else 0,
                                    speed=1.0, callback=callback)
            elapsed = (time.perf_counter() - start) * 1000
            path = output / f"{item['id']}-{repetition}.wav"
            sf.write(path, audio.samples, audio.sample_rate, subtype="PCM_16")
            duration = len(audio.samples) / audio.sample_rate * 1000
            row = {**item, "repetition": repetition, "first_invocation": not report["rows"],
                   "wav": str(path), "first_pcm_callback_ms": chunks[0]["at_ms"] if chunks else None,
                   "generation_ms": elapsed, "audio_ms": duration, "rtf": elapsed / duration if duration else None,
                   "sample_rate": audio.sample_rate, "chunks": chunks}
            report["rows"].append(row)
            save(output, report)
            print(json.dumps({key: row[key] for key in ["id", "repetition", "first_pcm_callback_ms", "generation_ms", "rtf"]}), flush=True)
    report["warm_summary"] = summarize([row for row in report["rows"] if not row["first_invocation"]], ["first_pcm_callback_ms", "generation_ms", "rtf"])
    save(output, report)


def asr(args, output):
    corpus = json.loads(args.corpus.read_text(encoding="utf-8-sig"))
    model = args.model.resolve()
    start = time.perf_counter()
    recognizer = sherpa_onnx.OnlineRecognizer.from_transducer(
        tokens=str(model / "tokens.txt"), encoder=str(model / "encoder-epoch-99-avg-1.int8.onnx"),
        decoder=str(model / "decoder-epoch-99-avg-1.onnx"), joiner=str(model / "joiner-epoch-99-avg-1.int8.onnx"),
        num_threads=args.threads, sample_rate=16000, enable_endpoint_detection=False,
        decoding_method="greedy_search", provider="cpu")
    report = {"test_type": "zipformer_bilingual_int8_synthetic_audio", "runtime": sherpa_onnx.__version__,
              "threads": args.threads, "model_load_ms": (time.perf_counter() - start) * 1000,
              "microphone": False, "child_speech_verified": False, "real_time_paced": args.pace,
              "chunk_ms": 20, "flush_padding_ms": 600, "rows": []}
    for repetition in range(args.repeats):
        for item in corpus["samples"]:
            samples, rate = sf.read(item["wav"], dtype="float32")
            if rate != 16000 or samples.ndim != 1:
                raise ValueError("Expected 16kHz mono fixture")
            stream = recognizer.create_stream()
            updates = []
            start = time.perf_counter()
            previous = ""
            for offset in range(0, len(samples), 320):
                end = min(offset + 320, len(samples))
                if args.pace:
                    time.sleep(max(0, end / rate - (time.perf_counter() - start)))
                stream.accept_waveform(rate, samples[offset:end])
                while recognizer.is_ready(stream):
                    recognizer.decode_stream(stream)
                text = recognizer.get_result(stream)
                if text and text != previous:
                    updates.append({"at_ms": (time.perf_counter() - start) * 1000, "audio_fed_ms": end / 16, "text": text})
                    previous = text
            last_audio = time.perf_counter()
            stream.accept_waveform(rate, np.zeros(9600, dtype=np.float32))
            stream.input_finished()
            while recognizer.is_ready(stream):
                recognizer.decode_stream(stream)
            transcript = recognizer.get_result(stream)
            finished = time.perf_counter()
            metric_language = "zh-en" if item["id"].startswith("mixed_") else item["language"]
            ref = units(item["text"], metric_language)
            actual = units(transcript, metric_language)
            row = {"id": item["id"], "language": item["language"], "repetition": repetition,
                   "reference": item["text"], "transcript": transcript,
                   "first_partial_ms": updates[0]["at_ms"] if updates else None,
                   "first_partial_audio_fed_ms": updates[0]["audio_fed_ms"] if updates else None,
                   "flush_compute_ms": (finished - last_audio) * 1000,
                   "elapsed_ms": (finished - start) * 1000, "audio_ms": len(samples) / 16,
                   "token_error_rate": distance(ref, actual) / len(ref) if ref else None,
                   "error_metric": "English words; Chinese characters plus Latin words; punctuation excluded",
                   "matched_terms": [term for term in item["terms"] if term.lower() in transcript.lower()],
                   "expected_terms": item["terms"], "partial_updates": updates}
            report["rows"].append(row)
            save(output, report)
            print(json.dumps({key: row[key] for key in ["id", "first_partial_ms", "flush_compute_ms", "token_error_rate"]}), flush=True)
    report["summary"] = summarize(report["rows"], ["first_partial_ms", "flush_compute_ms", "token_error_rate"])
    save(output, report)


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("mode", choices=["tts", "asr"])
    parser.add_argument("--model", type=Path, required=True)
    parser.add_argument("--corpus", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--threads", type=int, default=2)
    parser.add_argument("--repeats", type=int, default=1)
    parser.add_argument("--pace", action="store_true")
    args = parser.parse_args()
    if not 1 <= args.threads <= 8 or not 1 <= args.repeats <= 10:
        parser.error("Bounded baseline expects 1..8 threads and 1..10 repetitions")
    args.output.mkdir(parents=True, exist_ok=False)
    {"tts": tts, "asr": asr}[args.mode](args, args.output.resolve())
