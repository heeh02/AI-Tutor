"""Independent, offline-only FP32 ONNX diagnostic for the frozen synthetic E5 v2 set.

--prepare-only loads the local tokenizer and records all 41 inputs, without loading
an ONNX model. --run-model additionally runs only the already-present FP32 model
on CPU. Neither mode installs packages, downloads assets, or opens a learner DB.
"""
import argparse
import hashlib
import importlib.metadata
import json
import os
from pathlib import Path
import platform
import sys
import time

# Set before importing Transformers. No GPU framework or network fallback is needed.
os.environ["HF_HUB_OFFLINE"] = "1"
os.environ["TRANSFORMERS_OFFLINE"] = "1"
os.environ["HF_DATASETS_OFFLINE"] = "1"
os.environ["HF_HUB_DISABLE_TELEMETRY"] = "1"
os.environ["USE_TORCH"] = "0"
os.environ["USE_TF"] = "0"
os.environ["TOKENIZERS_PARALLELISM"] = "false"

FIXTURE_SHA = "0392be52062844b0dd0e558c500d5afe9c4f38b1cad923a6092e5fda125f89d7"
REVISION = "614241f622f53c4eeff9890bdc4f31cfecc418b3"
FP32_SHA = "ca456c06b3a9505ddfd9131408916dd79290368331e7d76bb621f1cba6bc8665"
TOKENIZER_SHA = "0b44a9d7b51c3c62626640cda0e2c2f70fdacdc25bbbd68038369d14ebdf4c39"
LOCAL_CONFIG_HASHES = {
    "config.json": "69137736cab8b8903a07fe8afaafdda25aac55415a12a55d1bffa9f581abf959",
    "tokenizer_config.json": "a1d6bc8734a6f635dc158508bef000f8e2e5a759c7d92f984b2c86e5ff53425b",
    "special_tokens_map.json": "d05497f1da52c5e09554c0cd874037a083e1dc1b9cfd48034d1c717f1afc07a7",
    "1_Pooling/config.json": "987f7a67a38fa564c849bb5d277c52ab9088a84368fc0be31a354125aebb12a0",
}


def sha_file(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def load_json(path):
    return json.loads(path.read_text(encoding="utf-8-sig"))


def require(condition, reason):
    if not condition:
        raise ValueError(reason)


def dependencies():
    packages = ["numpy", "onnxruntime", "transformers", "tokenizers", "sentencepiece"]
    return {name: importlib.metadata.version(name) for name in packages}


def checked_sources(root):
    fixture_path = root / "rust/ai-tutor/tests/fixtures/e5_education_recall_v2.json"
    directory = root / "models/memory/e5-small"
    files = {"tokenizer.json": TOKENIZER_SHA, **LOCAL_CONFIG_HASHES}
    require(sha_file(fixture_path) == FIXTURE_SHA, "Frozen v2 fixture changed; no relabeling or substitution allowed")
    fixture = load_json(fixture_path)
    require(fixture["synthetic_only"] is True and len(fixture["documents"]) == 16
            and len(fixture["cases"]) == 25, "Unexpected synthetic fixture contract")
    for name, expected in files.items():
        require(sha_file(directory / name) == expected, "Required local tokenizer/config missing or changed: " + name)
    return fixture, directory, files


def tokenize_all(fixture, directory):
    from transformers import AutoTokenizer
    from tokenizers import Tokenizer
    tokenizer = AutoTokenizer.from_pretrained(str(directory), local_files_only=True,
                                              use_fast=True, trust_remote_code=False)
    require(tokenizer.is_fast, "Expected the local fast tokenizer; no slow-tokenizer fallback")
    # A second view of the frozen JSON helps identify wrapper configuration changes.
    # This is Python tokenizers, not a claim that the Rust binary exported its IDs.
    raw = Tokenizer.from_file(str(directory / "tokenizer.json"))
    raw.no_padding()
    raw.no_truncation()
    rows = []
    sources = [("passage", document["id"], document["text"]) for document in fixture["documents"]]
    sources.extend(("query", case["id"], case["query"]) for case in fixture["cases"])
    for role, identifier, text in sources:
        prefixed = role + ": " + text
        encoded = tokenizer(prefixed, add_special_tokens=True, padding=False, truncation=False,
                            return_attention_mask=True, return_token_type_ids=True)
        ids = encoded["input_ids"]
        require(1 <= len(ids) <= 512, "No silent token truncation is allowed")
        require(len(encoded["attention_mask"]) == len(ids)
                and len(encoded["token_type_ids"]) == len(ids), "Token array length mismatch")
        direct = raw.encode(prefixed, add_special_tokens=True)
        direct_inputs = {"input_ids": direct.ids, "attention_mask": direct.attention_mask,
                         "token_type_ids": direct.type_ids}
        tokens = tokenizer.convert_ids_to_tokens(ids)
        rows.append({"key": role + ":" + identifier, "id": identifier, "role": role,
                     "prefixed_utf8_sha256": hashlib.sha256(prefixed.encode("utf-8")).hexdigest(),
                     "token_count": len(ids), "inputs": {key: encoded[key] for key in direct_inputs},
                     "first_token_ids": ids[:4], "last_token_ids": ids[-4:],
                     "first_tokens": tokens[:4], "last_tokens": tokens[-4:],
                     "unk_token_count": ids.count(tokenizer.unk_token_id),
                     "raw_json_tokenizer_inputs": direct_inputs,
                     "autotokenizer_matches_raw_json": all(encoded[key] == direct_inputs[key] for key in direct_inputs)})
    return rows, {"class": type(tokenizer).__name__, "is_fast": tokenizer.is_fast,
                  "local_files_only": True, "add_special_tokens": True,
                  "padding": False, "truncation": False, "explicit_return_token_type_ids": True,
                  "bos_token_id": tokenizer.bos_token_id, "eos_token_id": tokenizer.eos_token_id,
                  "pad_token_id": tokenizer.pad_token_id, "unk_token_id": tokenizer.unk_token_id}


def ranking_metrics(rank, relevant):
    if not relevant:
        return {"positive_recall": "not_applicable", "candidate_returned": bool(rank),
                "abstention": "not_implemented"}
    first = next((index + 1 for index, name in enumerate(rank) if name in relevant), None)
    found = sum(name in rank[:3] for name in relevant)
    return {"hit_at_1": bool(rank and rank[0] in relevant), "hit_at_3": found > 0,
            "recall_at_3": found / len(relevant), "all_required_at_3": found == len(relevant),
            "reciprocal_rank_at_5": 1 / first if first is not None and first <= 5 else 0.0,
            "reciprocal_rank_full_candidate_pool": 1 / first if first else 0.0}


def run_fp32(directory, rows):
    import numpy as np
    import onnxruntime as ort
    model_path = directory / "onnx/model.onnx"
    require(sha_file(model_path) == FP32_SHA, "Existing FP32 ONNX hash mismatch; no model download is permitted")
    options = ort.SessionOptions()
    options.intra_op_num_threads = 2
    options.inter_op_num_threads = 1
    options.execution_mode = ort.ExecutionMode.ORT_SEQUENTIAL
    start = time.perf_counter()
    session = ort.InferenceSession(str(model_path), sess_options=options, providers=["CPUExecutionProvider"])
    load_ms = (time.perf_counter() - start) * 1000
    names = [item.name for item in session.get_inputs()]
    require(set(names) == {"input_ids", "attention_mask", "token_type_ids"}, "Unexpected ONNX input contract")
    require("last_hidden_state" in [item.name for item in session.get_outputs()], "Missing token-level output")
    vectors = {}
    for row in rows:
        feed = {name: np.asarray([row["inputs"][name]], dtype=np.int64) for name in names}
        start = time.perf_counter()
        hidden = session.run(["last_hidden_state"], feed)[0]
        elapsed_ms = (time.perf_counter() - start) * 1000
        require(hidden.shape == (1, row["token_count"], 384), "Unexpected hidden shape")
        require(np.isfinite(hidden).all(), "Non-finite hidden values")
        # Official attention-mask mean then L2; independent NumPy reduction.
        mask = feed["attention_mask"].astype(bool)[..., None]
        pooled = np.where(mask, hidden, np.float32(0)).sum(axis=1, dtype=np.float32)
        pooled /= feed["attention_mask"].sum(axis=1, keepdims=True).astype(np.float32)
        norm = np.sqrt(np.square(pooled.astype(np.float64)).sum(axis=1, keepdims=True))
        require(bool(np.all(norm > 0)), "Zero pooled vector")
        vector = (pooled.astype(np.float64) / norm).astype(np.float32)[0]
        vectors[row["key"]] = vector
        row["fp32"] = {"hidden_shape": list(hidden.shape),
                       "hidden_sha256_f32le": hashlib.sha256(hidden.astype("<f4", copy=False).tobytes()).hexdigest(),
                       "hidden_coordinates": {"first_token_dims_0_7": hidden[0, 0, :8].tolist(),
                                              "last_token_dims_0_7": hidden[0, -1, :8].tolist()},
                       "pooled_vector": vector.tolist(),
                       "pooled_sha256_f32le": hashlib.sha256(vector.astype("<f4", copy=False).tobytes()).hexdigest(),
                       "pooled_l2": float(np.linalg.norm(vector.astype(np.float64))),
                       "inference_ms": elapsed_ms}
    return vectors, {"version": ort.__version__, "build_info": ort.get_build_info(),
                     "providers": session.get_providers(), "intra_threads": 2, "inter_threads": 1,
                     "model_sha256": FP32_SHA, "load_ms": load_ms,
                     "inputs": [{"name": v.name, "shape": v.shape, "type": v.type} for v in session.get_inputs()],
                     "outputs": [{"name": v.name, "shape": v.shape, "type": v.type} for v in session.get_outputs()]}


def compare_rankings(fixture, vectors, baseline):
    import numpy as np
    rows = []
    previous = {case["id"]: case for case in baseline["cases"]}
    for case in fixture["cases"]:
        query = vectors["query:" + case["id"]].astype(np.float64)
        ranking = [{"id": name, "cosine": float(np.dot(query, vectors["passage:" + name].astype(np.float64)))}
                   for name in fixture["candidate_pool"]]
        ranking.sort(key=lambda row: (-row["cosine"], row["id"]))
        ids = [row["id"] for row in ranking]
        first = previous[case["id"]]["dense"]["ranking"]
        first_ids = [row["id"] for row in first]
        first_scores = {row["id"]: row["cosine"] for row in first}
        rows.append({"id": case["id"], "group_unchanged": case["group"], "relevant": case["relevant"],
                     "fp32_python_ranking": ranking, "metrics": ranking_metrics(ids, case["relevant"]),
                     "rust_first_precision": baseline["model"]["precision"], "rust_first_dense_ranked_ids": first_ids,
                     "top1_matches_rust_first": ids[:1] == first_ids[:1],
                     "top3_order_matches_rust_first": ids[:3] == first_ids[:3],
                     "full_order_matches_rust_first": ids == first_ids,
                     "cosine_delta_from_rust_first": [{"id": row["id"], "python_fp32_minus_rust_first": row["cosine"] - first_scores[row["id"]]} for row in ranking]})
    positives = [row for row in rows if row["relevant"]]
    return rows, {"positive_cases": len(positives),
                  "hit_at_1": sum(row["metrics"]["hit_at_1"] for row in positives),
                  "hit_at_3": sum(row["metrics"]["hit_at_3"] for row in positives),
                  "all_required_at_3": sum(row["metrics"]["all_required_at_3"] for row in positives),
                  "mrr_at_5": sum(row["metrics"]["reciprocal_rank_at_5"] for row in positives) / len(positives),
                  "negative_cases": len(rows) - len(positives),
                  "top1_changed_vs_rust_first": sum(not row["top1_matches_rust_first"] for row in rows)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--prepare-only", action="store_true")
    mode.add_argument("--run-model", action="store_true")
    parser.add_argument("--load-note", default="not recorded; do not use timings as a performance baseline")
    args = parser.parse_args()
    root = args.root.resolve()
    require(not args.output.exists(), "Use a new report path; previous evidence must remain intact")
    versions = dependencies()
    fixture, directory, sources = checked_sources(root)
    rows, tokenizer = tokenize_all(fixture, directory)
    baseline_path = root / "runtime/goal-baselines/20261104/e5-education-first/report.json"
    baseline = load_json(baseline_path)
    require(baseline["fixture_sha256"] == FIXTURE_SHA, "Rust first report used a different fixture")
    prior_queries = {case["id"]: case for case in baseline["cases"]}
    counts = [{"id": row["id"], "python_tokens": row["token_count"],
               "rust_first_tokens": prior_queries[row["id"]]["query_tokens"],
               "equal": row["token_count"] == prior_queries[row["id"]]["query_tokens"]}
              for row in rows if row["role"] == "query"]
    report = {"probe": "offline_python_e5_fp32_reference", "mode": "run_model" if args.run_model else "prepare_only",
              "fixture_sha256": FIXTURE_SHA, "fixture_version": fixture["fixture_version"],
              "model_id": "intfloat/multilingual-e5-small", "revision": REVISION,
              "script_sha256": sha_file(Path(__file__)), "dependencies": versions,
              "python": sys.version, "platform": platform.platform(), "load_note": args.load_note,
              "tokenizer": tokenizer, "local_source_hashes": sources,
              "rust_first_report_sha256": sha_file(baseline_path), "query_token_count_comparison": counts,
              "all_wrapper_and_raw_tokenizer_inputs_equal": all(row["autotokenizer_matches_raw_json"] for row in rows),
              "items": rows, "model_executed": False, "network_used": False, "downloads": False,
              "devices_used": False, "learner_db_opened": False, "fixture_changed": False,
              "limitations": ["All 41 inputs are fixed synthetic texts; no labels, thresholds or defaults are changed",
                              "Rust first contains token counts and ranks, not token IDs or hidden/pooled tensors; exact Rust tensor parity is not claimed",
                              "Python AutoTokenizer and raw tokenizer.json are compared explicitly; both use the installed Python tokenizers package",
                              "First Rust report used INT8; Python uses FP32, so differences mix precision and runtime/frontend effects",
                              "Python ORT on Linux and the Rust Windows ORT DLL may use different versions/kernels; record both rather than assume bit equality",
                              "No PyTorch encoder-weight reference has been executed; agreement cannot prove the ONNX export equals original weights",
                              "Original group labels including q01 are retained; q01 is English-to-English, not a cross-language example"]}
    if args.run_model:
        vectors, runtime = run_fp32(directory, rows)
        cases, aggregate = compare_rankings(fixture, vectors, baseline)
        report.update(model_executed=True, runtime=runtime, cases=cases, aggregate=aggregate)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open("x", encoding="utf-8", newline="\n") as stream:
        json.dump(report, stream, ensure_ascii=False, indent=2, allow_nan=False)
        stream.write("\n")
    print(json.dumps({"report_written": True, "items": len(rows), "model_executed": report["model_executed"],
                      "token_count_mismatches": sum(not row["equal"] for row in counts),
                      "wrapper_raw_mismatches": sum(not row["autotokenizer_matches_raw_json"] for row in rows)}))


if __name__ == "__main__":
    main()
