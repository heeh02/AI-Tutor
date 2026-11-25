"""Independent, file-only streaming ASR comparison; no service or audio devices.

Use the same 16 kHz mono PCM16 corpus for both model kinds. Nonpaced runs
measure transcription; paced runs separately measure model-side latency.
"""
import argparse
import hashlib
import importlib.metadata
import json
from pathlib import Path
import platform
import re
import statistics
import time
import wave

import numpy as np
import sherpa_onnx


CRITICAL = {
    "zh_correction": ["不对", "还没有", "二十厘米"],
    "en_correction": ["not", "twenty centimeters"],
    "zh_hint": ["不要"],
    "zh_number": ["三点五秒", "两秒"],
    "en_number": ["three point five seconds", "two seconds"],
}


def digest(path):
    value = hashlib.sha256()
    with path.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            value.update(chunk)
    return value.hexdigest()


def units(text):
    # Keep numbers literal: twenty/20 and 二十/20 remain different spellings.
    return re.findall(r"[\u3400-\u9fff]|[a-z]+|[0-9]+", text.lower())


def distance(reference, actual):
    previous = list(range(len(actual) + 1))
    for i, expected in enumerate(reference):
        current = [i + 1]
        for j, observed in enumerate(actual):
            current.append(min(current[-1] + 1, previous[j + 1] + 1,
                               previous[j] + (expected != observed)))
        previous = current
    return previous[-1]


def contains_term(text, term):
    sequence, needle = units(text), units(term)
    return bool(needle) and any(sequence[i:i + len(needle)] == needle
                               for i in range(len(sequence) - len(needle) + 1))


def first_stable_partial(updates, final):
    """First nonempty normalized partial retained as a prefix at every later update.

    This is an after-the-fact metric, never a claim the online decoder knows
    which words are stable. Only pre-final real-audio updates are candidates.
    """
    sequences = [units(update["text"]) for update in updates] + [units(final)]
    for i, sequence in enumerate(sequences[:-1]):
        if sequence and all(later[:len(sequence)] == sequence for later in sequences[i + 1:]):
            return updates[i]["at_ms"]
    return None


def read_fixture(item, corpus_path):
    path = Path(item["wav"])
    if not path.is_absolute():
        path = corpus_path.parent / path
    path = path.resolve(strict=True)
    with wave.open(str(path), "rb") as source:
        if (source.getnchannels(), source.getsampwidth(), source.getframerate()) != (1, 2, 16000):
            raise ValueError(f"{item['id']}: expected mono PCM16 at 16000 Hz")
        pcm = source.readframes(source.getnframes())
    if not pcm or len(pcm) > 16000 * 2 * 60:
        raise ValueError(f"{item['id']}: fixture must be nonempty and at most 60 seconds")
    file_hash = digest(path)
    if item.get("sha256") and item["sha256"].lower() != file_hash:
        raise ValueError(f"{item['id']}: corpus hash differs from WAV")
    return np.frombuffer(pcm, dtype="<i2").astype(np.float32) / 32768, {
        "path": str(path), "sha256": file_hash,
        "original_sha256": item.get("original_sha256"),
        "pcm_sha256": hashlib.sha256(pcm).hexdigest(), "samples": len(pcm) // 2,
    }


def model_files(kind):
    if kind == "paraformer":
        return ["tokens.txt", "encoder.int8.onnx", "decoder.int8.onnx"]
    return ["tokens.txt", "encoder-epoch-99-avg-1.int8.onnx",
            "decoder-epoch-99-avg-1.onnx", "joiner-epoch-99-avg-1.int8.onnx"]


def load_recognizer(kind, model, threads):
    common = dict(tokens=str(model / "tokens.txt"), num_threads=threads,
                  sample_rate=16000, feature_dim=80, provider="cpu",
                  enable_endpoint_detection=False, decoding_method="greedy_search")
    if kind == "paraformer":
        return sherpa_onnx.OnlineRecognizer.from_paraformer(
            encoder=str(model / "encoder.int8.onnx"),
            decoder=str(model / "decoder.int8.onnx"), **common)
    return sherpa_onnx.OnlineRecognizer.from_transducer(
        encoder=str(model / "encoder-epoch-99-avg-1.int8.onnx"),
        decoder=str(model / "decoder-epoch-99-avg-1.onnx"),
        joiner=str(model / "joiner-epoch-99-avg-1.int8.onnx"), **common)


def decode(recognizer, samples, paced, padding_ms, final_option=False):
    stream = recognizer.create_stream()  # Each utterance has independent model caches.
    updates = []
    previous = ""
    max_feed_lateness_ms = 0.0
    start = time.perf_counter()
    for offset in range(0, len(samples), 320):
        end = min(offset + 320, len(samples))
        deadline = start + end / 16000
        if paced:
            time.sleep(max(0.0, deadline - time.perf_counter()))
            max_feed_lateness_ms = max(max_feed_lateness_ms, (time.perf_counter() - deadline) * 1000)
        stream.accept_waveform(16000, samples[offset:end])
        while recognizer.is_ready(stream):
            recognizer.decode_stream(stream)
        text = recognizer.get_result(stream)
        if text != previous:
            updates.append({"at_ms": (time.perf_counter() - start) * 1000,
                            "audio_fed_ms": end / 16, "text": text})
            previous = text
    last_real_audio = time.perf_counter()
    # Artificial tail is delivered immediately. This is flush compute time,
    # not waiting for endpoint silence or an HTTP final request.
    stream.accept_waveform(16000, np.zeros(16 * padding_ms, dtype=np.float32))
    stream.input_finished()
    if final_option:
        # v2.0.1 OnlineRecognizerParaformerImpl explicitly supports a final
        # short feature chunk through this option. input_finished alone does
        # not set it. Leave transducer behavior unchanged.
        stream.set_option("is_final", "1")
    while recognizer.is_ready(stream):
        recognizer.decode_stream(stream)
    final = recognizer.get_result(stream)
    finished = time.perf_counter()
    first = next((update for update in updates if update["text"]), None)
    return final, {
        "first_partial_ms": first["at_ms"] if first else None,
        "first_partial_audio_fed_ms": first["audio_fed_ms"] if first else None,
        "first_stable_prefix_ms": first_stable_partial(updates, final),
        "partial_update_count": len(updates), "partial_updates": updates,
        "prefix_revision_count": sum(not units(b["text"])[:len(units(a["text"]))] == units(a["text"])
                                     for a, b in zip(updates, updates[1:])),
        "final_flush_compute_ms": (finished - last_real_audio) * 1000,
        "final_changed_text": final != previous,
        "total_elapsed_ms": (finished - start) * 1000,
        "audio_ms": len(samples) / 16,
        "max_feed_lateness_ms": max_feed_lateness_ms if paced else None,
    }


def summarize(rows, paced):
    result = {}
    for group in ["zh", "en", "zh-en", "all"]:
        selected = [row for row in rows if group == "all" or row["metric_group"] == group]
        if not selected:
            continue
        counts = {"samples": len(selected), "reference_units": sum(r["reference_units"] for r in selected),
                  "edit_distance": sum(r["edit_distance"] for r in selected),
                  "term_hits": sum(len(r["matched_terms"]) for r in selected),
                  "term_total": sum(len(r["expected_terms"]) for r in selected),
                  "critical_hits": sum(len(r["matched_critical_terms"]) for r in selected),
                  "critical_total": sum(len(r["critical_terms"]) for r in selected)}
        counts["micro_error_rate"] = counts["edit_distance"] / counts["reference_units"]
        if paced:
            for name in ["first_partial_ms", "first_stable_prefix_ms", "final_flush_compute_ms", "max_feed_lateness_ms"]:
                values = [row[name] for row in selected if row[name] is not None]
                counts[name] = {"n": len(values), "min": min(values), "median": statistics.median(values),
                                "max": max(values)} if values else None
        result[group] = counts
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model-kind", choices=["paraformer", "zipformer"], required=True)
    parser.add_argument("--model", type=Path, required=True)
    parser.add_argument("--corpus", type=Path, required=True)
    parser.add_argument("--corpus-label", required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--load-note", required=True, help="Observed other work; timing limitations")
    parser.add_argument("--pace", action="store_true")
    parser.add_argument("--threads", type=int, default=2)
    parser.add_argument("--padding-ms", type=int, default=660)
    args = parser.parse_args()
    if args.threads != 2 or args.padding_ms not in [600, 660]:
        parser.error("This comparison fixes CPU threads to 2 and permits explicit 600/660 ms flush")
    model, corpus_path = args.model.resolve(strict=True), args.corpus.resolve(strict=True)
    corpus = json.loads(corpus_path.read_text(encoding="utf-8-sig"))
    items = corpus["samples"]
    if not 1 <= len(items) <= 24 or len({item["id"] for item in items}) != len(items):
        raise ValueError("Expected 1..24 uniquely named authored fixtures")
    fixtures = [read_fixture(item, corpus_path) for item in items]
    files = [{"name": name, "sha256": digest(model / name), "bytes": (model / name).stat().st_size}
             for name in model_files(args.model_kind)]
    args.output.mkdir(parents=True, exist_ok=False)
    start = time.perf_counter()
    recognizer = load_recognizer(args.model_kind, model, args.threads)
    load_ms = (time.perf_counter() - start) * 1000
    # Silence warms operator allocation without giving either model corpus text.
    final_option = args.model_kind == "paraformer"
    decode(recognizer, np.zeros(16000, dtype=np.float32), False, args.padding_ms, final_option)
    report = {
        "protocol_version": 1, "experiment": "independent_synthetic_streaming_asr_comparison",
        "model_kind": args.model_kind, "model_files": files, "model_load_ms": load_ms,
        "model_manifest": str(model / "download-manifest.json") if (model / "download-manifest.json").exists() else None,
        "runtime": {"python": platform.python_version(), "sherpa_onnx": sherpa_onnx.__version__,
                    "numpy": np.__version__, "sherpa_onnx_core": importlib.metadata.version("sherpa-onnx-core")},
        "provider": "cpu", "threads": 2, "corpus_label": args.corpus_label,
        "corpus_path": str(corpus_path), "corpus_sha256": digest(corpus_path),
        "mode": "paced_20ms" if args.pace else "nonpaced_quality_only",
        "load_note": args.load_note, "model_latency_claims_allowed": args.pace,
        "chunk_ms": 20, "final_padding_ms": args.padding_ms, "padding_is_paced": False,
        "paraformer_final_option": final_option,
        "endpoint_detection": False, "decoding": "greedy_search", "warmup": "one second zero PCM independent stream",
        "microphone": False, "speaker_output": False, "http_ui_latency_measured": False,
        "limitations": "Adult synthesized fixtures only. No child, accent, noise or naturalness evidence. Qwen roundtrip differences combine TTS, resampling and ASR, not TTS error attribution. Historical ONNX export is not verified identical to author fd2af606 snapshot; publisher license metadata does not complete upstream license chain.",
        "metric_definition": "English Latin words; Chinese Han characters; mixed Han characters + Latin words; lowercase and punctuation excluded. Digits are literal tokens; no numeric/semantic normalization. Stable prefix is determined retrospectively across later updates and final. No population P95 inferred.",
        "rows": [],
    }
    for item, (samples, fixture) in zip(items, fixtures):
        final, measured = decode(recognizer, samples, args.pace, args.padding_ms, final_option)
        reference, actual = units(item["text"]), units(final)
        if not reference:
            raise ValueError("Reference must contain scored units")
        terms, critical = item.get("terms", []), CRITICAL.get(item["id"], [])
        row = {"id": item["id"], "language": item["language"],
               "metric_group": "zh-en" if item["id"].startswith("mixed_") else item["language"],
               "fixture": fixture, "reference": item["text"], "transcript": final,
               "reference_units": len(reference), "edit_distance": distance(reference, actual),
               "expected_terms": terms, "matched_terms": [term for term in terms if contains_term(final, term)],
               "critical_terms": critical, "matched_critical_terms": [term for term in critical if contains_term(final, term)],
               **measured}
        row["error_rate"] = row["edit_distance"] / row["reference_units"]
        report["rows"].append(row)
        report["summary"] = summarize(report["rows"], args.pace)
        (args.output / "report.json").write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
        print(json.dumps({"id": row["id"], "error_rate": row["error_rate"],
                          "first_partial_ms": row["first_partial_ms"] if args.pace else None}, ensure_ascii=True), flush=True)


if __name__ == "__main__":
    main()
