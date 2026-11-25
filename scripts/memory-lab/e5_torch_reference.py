"""One fixed-weight CPU PyTorch comparison against the existing 41-item ONNX report.

No installation or download occurs here. --prepare-only checks the verified local
weight and rechecks tokenization. Only --run-model loads and evaluates the encoder.
"""
import argparse
import importlib.metadata
import json
import os
from pathlib import Path
import platform
import sys
import time

import e5_python_reference as shared

# The shared tokenizer helper sets offline flags without importing Transformers.
# Enable Torch only in this new diagnostic process; GPU visibility stays empty.
os.environ["USE_TORCH"] = "1"
os.environ["CUDA_VISIBLE_DEVICES"] = ""

WEIGHT_SHA = "1a55775f53449dac10a2bcbc312469fac40b96d53198c407081a831f81c98477"
WEIGHT_BYTES = 470641600


def distribution(values):
    return {"count": len(values), "min": min(values), "max": max(values), "mean": sum(values) / len(values)}


def evaluate(directory, rows, onnx, fixture):
    import numpy as np
    import torch
    from transformers import AutoModel
    from torch.nn import functional as functional

    torch.set_num_threads(2)
    torch.set_num_interop_threads(1)
    started = time.perf_counter()
    model = AutoModel.from_pretrained(str(directory), local_files_only=True,
                                     trust_remote_code=False, use_safetensors=True,
                                     dtype=torch.float32, attn_implementation="eager")
    model.to(device="cpu", dtype=torch.float32)
    model.eval()
    shared.require(all(parameter.device.type == "cpu" and parameter.dtype == torch.float32
                       for parameter in model.parameters()), "Encoder must remain CPU float32")
    load_ms = (time.perf_counter() - started) * 1000
    previous = {row["key"]: row for row in onnx["items"]}
    vectors, deltas = {}, []
    with torch.inference_mode():
        for row in rows:
            earlier = previous[row["key"]]
            shared.require(row["inputs"] == earlier["inputs"], "Token inputs changed; do not repair or substitute the sample")
            # Feed the arrays already recorded by the independent ONNX run.
            feed = {key: torch.tensor([values], dtype=torch.long, device="cpu")
                    for key, values in earlier["inputs"].items()}
            start = time.perf_counter()
            hidden = model(**feed).last_hidden_state
            inference_ms = (time.perf_counter() - start) * 1000
            shared.require(tuple(hidden.shape) == (1, row["token_count"], 384), "Unexpected token-level output")
            shared.require(bool(torch.isfinite(hidden).all()), "Non-finite token-level output")
            masked = hidden.masked_fill(~feed["attention_mask"][..., None].bool(), 0.0)
            pooled = masked.sum(dim=1) / feed["attention_mask"].sum(dim=1)[..., None]
            vector = functional.normalize(pooled, p=2, dim=1)[0].cpu().numpy()
            hidden_array = hidden.cpu().numpy()
            reference = np.asarray(earlier["fp32"]["pooled_vector"], dtype=np.float64)
            actual = vector.astype(np.float64)
            difference = actual - reference
            vector_cosine = float(np.dot(actual, reference) / (np.linalg.norm(actual) * np.linalg.norm(reference)))
            delta = {"key": row["key"], "tokens_identical": True,
                     "normalized_vector_cosine": vector_cosine,
                     "max_abs_component_error": float(np.max(np.abs(difference))),
                     "component_rmse": float(np.sqrt(np.mean(np.square(difference))))}
            deltas.append(delta)
            vectors[row["key"]] = vector
            row["torch_fp32"] = {
                "hidden_shape": list(hidden_array.shape),
                "hidden_sha256_f32le": shared.hashlib.sha256(hidden_array.astype("<f4", copy=False).tobytes()).hexdigest(),
                "hidden_coordinates": {"first_token_dims_0_7": hidden_array[0, 0, :8].tolist(),
                                       "last_token_dims_0_7": hidden_array[0, -1, :8].tolist()},
                "pooled_vector": vector.tolist(),
                "pooled_sha256_f32le": shared.hashlib.sha256(vector.astype("<f4", copy=False).tobytes()).hexdigest(),
                "pooled_l2": float(np.linalg.norm(actual)), "inference_ms": inference_ms,
                "onnx_vector_comparison": delta,
            }
    prior_cases = {case["id"]: case for case in onnx["cases"]}
    cases, pair_deltas = [], []
    for case in fixture["cases"]:
        query = vectors["query:" + case["id"]].astype(np.float64)
        rank = [{"id": name, "cosine": float(np.dot(query, vectors["passage:" + name].astype(np.float64)))}
                for name in fixture["candidate_pool"]]
        rank.sort(key=lambda item: (-item["cosine"], item["id"]))
        ids = [item["id"] for item in rank]
        prior = prior_cases[case["id"]]["fp32_python_ranking"]
        prior_ids = [item["id"] for item in prior]
        prior_scores = {item["id"]: item["cosine"] for item in prior}
        pairs = [{"id": item["id"], "torch_minus_onnx_cosine": item["cosine"] - prior_scores[item["id"]]} for item in rank]
        pair_deltas.extend(abs(item["torch_minus_onnx_cosine"]) for item in pairs)
        cases.append({"id": case["id"], "group_unchanged": case["group"], "relevant": case["relevant"],
                      "torch_ranking": rank, "onnx_ranked_ids": prior_ids,
                      "top1_equal": ids[:1] == prior_ids[:1], "full_order_equal": ids == prior_ids,
                      "pair_deltas": pairs, "metrics": shared.ranking_metrics(ids, case["relevant"])})
    positives = [row for row in cases if row["relevant"]]
    aggregate = {"positive_cases": len(positives),
                 "hit_at_1": sum(row["metrics"]["hit_at_1"] for row in positives),
                 "hit_at_3": sum(row["metrics"]["hit_at_3"] for row in positives),
                 "all_required_at_3": sum(row["metrics"]["all_required_at_3"] for row in positives),
                 "mrr_at_5": sum(row["metrics"]["reciprocal_rank_at_5"] for row in positives) / len(positives),
                 "negative_cases": len(cases) - len(positives),
                 "full_order_equal_queries": sum(row["full_order_equal"] for row in cases),
                 "top1_changed_queries": sum(not row["top1_equal"] for row in cases)}
    return {"model_executed": True, "cases": cases, "aggregate": aggregate,
            "vector_comparisons": deltas,
            "vector_cosine_distribution": distribution([row["normalized_vector_cosine"] for row in deltas]),
            "vector_rmse_distribution": distribution([row["component_rmse"] for row in deltas]),
            "retrieval_pair_abs_cosine_delta_distribution": distribution(pair_deltas),
            "runtime": {"model_class": type(model).__name__, "torch_version": torch.__version__,
                        "device": "cpu", "dtype": "float32", "intra_threads": torch.get_num_threads(),
                        "inter_threads": torch.get_num_interop_threads(), "training": model.training,
                        "inference_mode": True, "attention_implementation": "eager", "load_ms": load_ms,
                        "pooling": "attention-mask mean including special tokens, then torch F.normalize p=2",
                        "inference_total_ms": sum(row["torch_fp32"]["inference_ms"] for row in rows)}}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--load-note", default="not recorded; timing is not a performance baseline")
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--prepare-only", action="store_true")
    mode.add_argument("--run-model", action="store_true")
    args = parser.parse_args()
    shared.require(not args.output.exists(), "Use a new report path; preserve earlier evidence")
    root = args.root.resolve()
    fixture, directory, sources = shared.checked_sources(root)
    weight = directory / "model.safetensors"
    shared.require(weight.stat().st_size == WEIGHT_BYTES and shared.sha_file(weight) == WEIGHT_SHA,
                   "The fixed native weight is missing or changed; no download fallback")
    onnx_path = root / "runtime/goal-baselines/20261104/e5-python-reference/report.json"
    onnx = shared.load_json(onnx_path)
    shared.require(onnx["fixture_sha256"] == shared.FIXTURE_SHA
                   and onnx["runtime"]["model_sha256"] == shared.FP32_SHA, "ONNX baseline provenance differs")
    rows, tokenizer = shared.tokenize_all(fixture, directory)
    previous = {row["key"]: row for row in onnx["items"]}
    shared.require(len(rows) == len(previous) == 41, "All 41 fixed inputs are required")
    shared.require(all(row["inputs"] == previous[row["key"]]["inputs"] for row in rows),
                   "Tokenization changed before execution; do not substitute inputs")
    report = {"probe": "offline_e5_native_torch_reference", "status": "prepared",
              "mode": "run_model" if args.run_model else "prepare_only", "model_executed": False,
              "fixture_sha256": shared.FIXTURE_SHA, "revision": shared.REVISION,
              "model_id": "intfloat/multilingual-e5-small", "weight_sha256": WEIGHT_SHA,
              "weight_bytes": WEIGHT_BYTES, "license": "MIT",
              "weight_source_url": "https://huggingface.co/intfloat/multilingual-e5-small/resolve/" + shared.REVISION + "/model.safetensors?download=true",
              "local_source_hashes": sources, "onnx_report_sha256": shared.sha_file(onnx_path),
              "script_sha256": shared.sha_file(Path(__file__)),
              "shared_helper_sha256": shared.sha_file(Path(shared.__file__)),
              "dependencies": {name: importlib.metadata.version(name) for name in ["torch", "transformers", "safetensors", "numpy", "tokenizers"]},
              "python": sys.version, "platform": platform.platform(), "load_note": args.load_note,
              "tokenizer": tokenizer, "all_41_token_inputs_equal_to_onnx": True, "items": rows,
              "network_used": False, "downloads": False, "packages_installed": False,
              "devices_used": False, "learner_db_opened": False, "production_recall_changed": False,
              "limitations": ["One native-weight diagnostic on the same fixed synthetic texts, not an independent held-out quality test",
                              "Eager CPU float32, eval and inference_mode are fixed before observing results; no result-driven tuning",
                              "Small floating-point differences across Torch and ORT are measured; bit equality is not required",
                              "Agreement validates this export path on these inputs, not general factual reliability or relevance acceptance",
                              "This is the final frontend diagnostic for this model; quality work moves to independent lexical shadow evaluation"]}
    failed = None
    if args.run_model:
        report["model_executed"] = None  # A failed attempt is not reported as "no inference".
        try:
            report.update(evaluate(directory, rows, onnx, fixture))
            report["status"] = "completed"
        except Exception as error:
            report.update(status="failed", error_type=type(error).__name__, error=str(error)[:1000],
                          execution_attempted=True,
                          completed_items=sum("torch_fp32" in row for row in rows))
            failed = error
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open("x", encoding="utf-8", newline="\n") as stream:
        json.dump(report, stream, ensure_ascii=False, indent=2, allow_nan=False)
        stream.write("\n")
    print(json.dumps({"report_written": True, "status": report["status"], "items": len(rows),
                      "model_executed": report["model_executed"]}))
    if failed is not None:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
