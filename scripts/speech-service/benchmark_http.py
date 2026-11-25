"""Paced synthetic utterances through the actual authenticated ASR HTTP service."""
import argparse
import json
from pathlib import Path
import time
from urllib.request import Request, build_opener, ProxyHandler
from uuid import uuid4
import wave


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--corpus", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    config = json.loads((args.root / "runtime/speech-service.json").read_text())
    corpus = json.loads(args.corpus.read_text(encoding="utf-8-sig"))
    opener = build_opener(ProxyHandler({}))

    def request(path, body, generation=None):
        headers = {"Authorization": "Bearer " + config["token"]}
        if isinstance(body, dict):
            headers["Content-Type"] = "application/json"
            body = json.dumps(body).encode()
        if generation is not None:
            headers["X-Recording-Generation"] = str(generation)
        with opener.open(Request(config["base_url"] + path, body, headers), timeout=4) as response:
            return json.loads(response.read())

    report = {"type": "paced_synthetic_audio_over_local_http", "microphone": False,
              "speaker_output": False, "ui_latency_measured": False, "rows": []}
    for generation, item in enumerate(corpus["samples"], 1):
        sid = str(uuid4())
        request("/v1/asr/start", dict(protocol_version=1, session_id=sid,
                recording_generation=generation, sample_rate=16000, channels=1,
                encoding="pcm_s16le", language="auto", max_duration_ms=20000))
        path = "/v1/asr/" + sid
        chunks, previous, partials, sequence = [], "", [], 0
        start = time.perf_counter()
        with wave.open(item["wav"], "rb") as wav:
            assert (wav.getnchannels(), wav.getsampwidth(), wav.getframerate()) == (1, 2, 16000)
            samples = 0
            while pcm := wav.readframes(1600):
                samples += len(pcm) // 2
                time.sleep(max(0, samples / 16000 - (time.perf_counter() - start)))
                called = time.perf_counter()
                response = request(path + f"/chunk?sequence={sequence}", pcm, generation)
                chunks.append((time.perf_counter() - called) * 1000)
                assert response["session_id"] == sid and response["sequence"] == sequence
                if response["text"] and response["text"] != previous:
                    partials.append({"at_ms": (time.perf_counter() - start) * 1000, "text": response["text"]})
                    previous = response["text"]
                sequence += 1
        called = time.perf_counter()
        final = request(path + "/final", dict(recording_generation=generation, sequence=sequence))
        final_ms = (time.perf_counter() - called) * 1000
        assert final["final"] and final["sequence"] == sequence and final["session_id"] == sid
        row = {"id": item["id"], "language": item["language"], "reference": item["text"],
               "transcript": final["text"], "first_partial_ms": partials[0]["at_ms"] if partials else None,
               "final_request_ms": final_ms, "max_chunk_request_ms": max(chunks),
               "partial_updates": partials, "chunks": sequence, "audio_ms": samples / 16}
        report["rows"].append(row)
        (args.output / "report.json").write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
        print(json.dumps({k: row[k] for k in ["id", "first_partial_ms", "final_request_ms", "max_chunk_request_ms"]}), flush=True)


if __name__ == "__main__":
    main()
