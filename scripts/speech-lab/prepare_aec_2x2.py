"""Freeze synthetic inputs and a complete AEC 2x2 protocol; never run AEC/models/devices."""
import argparse
import hashlib
import json
import math
from pathlib import Path
import platform
import shutil
import sys
import wave

import numpy as np
import scipy
from scipy.signal import resample_poly

FAR_IDS = ["zh_short", "en_concept", "zh_correction", "en_hint", "zh_number", "mixed_en"]
NEAR_IDS = ["en_short", "zh_concept", "en_correction", "zh_hint", "en_number", "mixed_zh"]
CASES = ["echo_only_20ms", "echo_only_80ms", "echo_only_160ms", "near_only",
         "double_talk_ser_-6db", "double_talk_ser_0db", "double_talk_ser_6db"]
HASH_FIELDS = ["render_sha256_f32le", "capture_sha256_f32le", "near_sha256_f32le", "echo_sha256_f32le"]
MODEL_REVISION = "85e237c12c027371202489a0ec509ded67b5e4b5"
RATE, COUNT = 16000, 40 * 16000


def digest(data):
    return hashlib.sha256(data).hexdigest()


def write_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("x", encoding="utf-8", newline="\n") as file:
        json.dump(value, file, ensure_ascii=False, indent=2, allow_nan=False)
        file.write("\n")


def read_pcm(path, rate):
    with wave.open(str(path), "rb") as file:
        if (file.getnchannels(), file.getsampwidth(), file.getframerate(), file.getcomptype()) != (1, 2, rate, "NONE"):
            raise ValueError("Unexpected fixed synthetic PCM format")
        return np.frombuffer(file.readframes(file.getnframes()), dtype="<i2").copy()


def rms(samples):
    # Deliberately match Rust's sequential f64 sum rather than a parallel NumPy reduction.
    return math.sqrt(sum(float(v) * float(v) for v in samples) / max(1, len(samples)))


def pcm_stats(pcm, rate):
    values = pcm.astype(np.float32) / 32768
    endpoint = min(len(pcm), rate // 50)
    return {"sample_rate": rate, "channels": 1, "encoding": "pcm_s16le", "samples": len(pcm),
            "duration_ms": len(pcm) * 1000 / rate, "rms": rms(values),
            "peak": float(np.max(np.abs(values))), "dc_mean": float(np.mean(values.astype(np.float64))),
            "first_sample_i16": int(pcm[0]), "last_sample_i16": int(pcm[-1]),
            "first_20ms_rms": rms(values[:endpoint]), "last_20ms_rms": rms(values[-endpoint:]),
            "full_scale_sample_count": int(np.count_nonzero((pcm == -32768) | (pcm == 32767)))}


def normalize(samples, target):
    samples *= np.float32(target / max(rms(samples), 1e-12))


def make_tape(samples, ids):
    return np.concatenate([part for name in ids for part in
                           (samples[name].astype(np.float32) / 32768, np.zeros(3200, np.float32))])


def frozen_mixtures(far, near):
    result = []
    for case in CASES:
        render = np.resize(far, COUNT).copy()
        normalize(render, 0.10)
        near_only = case == "near_only"
        delay = int(case.removeprefix("echo_only_").removesuffix("ms")) if case.startswith("echo_only_") else (0 if near_only else 80)
        if near_only:
            render.fill(0)
        echo = np.zeros(COUNT, np.float32)
        # Ordered f32 operations match the existing Rust three-tap synthetic echo.
        for offset, gain in [(0, 0.55), (192, 0.18), (432, -0.08)]:
            shift = delay * 16 + offset
            if shift == 0:
                echo += render * np.float32(gain)
            else:
                echo[shift:] += render[:-shift] * np.float32(gain)
        learner = np.zeros(COUNT, np.float32)
        if near_only or case.startswith("double_talk_"):
            start = 0 if near_only else 6 * RATE
            learner[start:] = np.resize(near, COUNT)[start:]
            ser = int(case.removeprefix("double_talk_ser_").removesuffix("db")) if not near_only else None
            target = 0.08 if near_only else rms(echo[start:]) * math.pow(10, ser / 20)
            normalize(learner[start:], target)
        capture = echo + learner
        peak = np.float32(max(float(np.max(np.abs(render))), float(np.max(np.abs(capture)))))
        common_gain = np.minimum(np.float32(0.8) / np.maximum(peak, np.float32(0.8)), np.float32(1))
        signals = {"render": render * common_gain, "capture": capture * common_gain,
                   "near": learner * common_gain, "echo": echo * common_gain}
        result.append({"scenario": case, "common_gain_to_avoid_clipping": float(common_gain),
                       **{name + "_sha256_f32le": digest(values.astype("<f4").tobytes()) for name, values in signals.items()}})
    return result


def prepare(root, output):
    sapi_manifest = root / "runtime/goal-baselines/20261103/sapi-corpus/manifest.json"
    qwen_report = root / "runtime/goal-baselines/20261104/qwen-chunk8-warm/report.json"
    prior_resampled = root / "runtime/goal-baselines/20261104/qwen-chunk8-asr-input/corpus.json"
    original_report = root / "runtime/goal-baselines/20261104/aec-offline-first/report.json"
    method_script = root / "scripts/speech-lab/prepare_tts_roundtrip.py"
    original = json.loads(original_report.read_text(encoding="utf-8"))
    sapi = json.loads(sapi_manifest.read_text(encoding="utf-8"))
    qwen = json.loads(qwen_report.read_text(encoding="utf-8"))
    prior = {row["id"]: row for row in json.loads(prior_resampled.read_text(encoding="utf-8"))["samples"]}
    if qwen["model_revision"] != MODEL_REVISION or qwen["speaker_output"] is not False:
        raise ValueError("Qwen source is not the fixed synthetic chunk8 report")
    if sapi["audible_output"] is not False or len(sapi["samples"]) != 12:
        raise ValueError("SAPI corpus is not the fixed synthetic source")
    source_hashes = []
    sapi_pcm, qwen_pcm = {}, {}
    sapi_records, qwen_records = [], []
    expected_sapi = {row["id"]: row["sha256"] for row in original["input"]["samples"]}
    output.mkdir(parents=True, exist_ok=False)
    for row in sapi["samples"]:
        name = row["id"]
        source = sapi_manifest.parent / f"{name}.wav"
        raw_hash = digest(source.read_bytes())
        if (row["source_type"] != "synthetic_sapi_adult_voice" or row["speaker_output"] is not False
                or raw_hash != row["sha256"] or raw_hash != expected_sapi[name]):
            raise ValueError("SAPI source provenance or hash changed")
        target = output / "audio/sapi16k" / source.name
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, target)
        sapi_pcm[name] = read_pcm(target, RATE)
        sapi_records.append({"id": name, "source_type": "fixed_synthetic_sapi_adult_voice",
                             "wav": target.relative_to(output).as_posix(), "sha256": raw_hash,
                             "original_wav": str(source), "original_sha256": raw_hash,
                             "copied_byte_for_byte": digest(target.read_bytes()) == raw_hash,
                             "signal": pcm_stats(sapi_pcm[name], RATE)})
        source_hashes.append((source, raw_hash))
    qwen_rows = {row["id"]: row for row in qwen["rows"]}
    for name in NEAR_IDS:
        row = qwen_rows[name]
        if not row["success"]:
            raise ValueError("A preselected Qwen source was not generated successfully")
        source = qwen_report.parent / row["wav"]
        raw_hash = digest(source.read_bytes())
        if raw_hash != prior[name]["original_sha256"]:
            raise ValueError("Original Qwen synthetic source changed")
        pcm = read_pcm(source, 24000)
        samples = pcm.astype(np.float32) / 32768
        filtered = resample_poly(samples, 2, 3, window=("kaiser", 5.0), padtype="constant", cval=0.0)
        rounded = np.rint(filtered * 32768)
        down = np.clip(rounded, -32768, 32767).astype("<i2")
        if len(down) != (len(pcm) * 2 + 2) // 3:
            raise ValueError("Resampler changed the declared endpoint convention")
        target = output / "audio/qwen16k" / f"{name}.wav"
        target.parent.mkdir(parents=True, exist_ok=True)
        with wave.open(str(target), "wb") as file:
            file.setparams((1, 2, RATE, 0, "NONE", "not compressed"))
            file.writeframes(down.tobytes())
        target_hash = digest(target.read_bytes())
        if target_hash != prior[name]["sha256"]:
            raise ValueError("Resampling differs from the previously frozen method output")
        qwen_pcm[name] = down
        qwen_records.append({"id": name, "source_type": "previously_generated_qwen_chunk8_waveform",
                             "wav": target.relative_to(output).as_posix(), "sha256": target_hash,
                             "original_wav": str(source), "original_sha256": raw_hash,
                             "voice": row["voice"], "model_revision": MODEL_REVISION,
                             "original_signal": pcm_stats(pcm, 24000), "signal": pcm_stats(down, RATE),
                             "round_then_clip_sample_count": int(np.count_nonzero((rounded < -32768) | (rounded > 32767))),
                             "matches_prior_resampling_sha256": True})
        source_hashes.append((source, raw_hash))

    far = make_tape(sapi_pcm, FAR_IDS)
    mixtures = {"sapi_near": frozen_mixtures(far, make_tape(sapi_pcm, NEAR_IDS)),
                "qwen_near": frozen_mixtures(far, make_tape(qwen_pcm, NEAR_IDS))}
    for expected, actual in zip(original["cases"][:7], mixtures["sapi_near"]):
        if expected["scenario"] != actual["scenario"] or any(expected["parameters"][key] != actual[key] for key in HASH_FIELDS):
            raise ValueError("Reconstruction changed an original ordinary-case input hash")
    baseline = [{"scenario": row["scenario"], **{key: row[key] for key in HASH_FIELDS}} for row in mixtures["sapi_near"]]
    baseline_hash = digest(json.dumps(baseline, sort_keys=True, separators=(",", ":")).encode())
    if baseline_hash != "514046b5375bc9516d1815fe4c69cb8afaf586fe8feb848c8f5c50c393700033":
        raise ValueError("Original seven-case input hash bundle changed")

    parameters = {"sample_rate": RATE, "frame_samples": 160, "duration_ms": 40000,
                  "inter_clip_silence_ms": 200, "repeat_tape_without_phase_reset": True,
                  "far_rms_before_common_scaling": 0.10, "near_only_rms_before_common_scaling": 0.08,
                  "near_onset_ms": 6000, "near_tape_phase": "absolute case sample index modulo near tape length",
                  "fir": [{"offset_samples": n, "gain": gain} for n, gain in [(0, .55), (192, .18), (432, -.08)]],
                  "device_delay_ms": [20, 80, 160], "double_talk_device_delay_ms": 80,
                  "double_talk_ser_db": [-6, 0, 6], "clip_policy": "common scalar on render, near, echo and capture; peak <=0.8",
                  "ordinary_scenario_order": CASES, "execution_order": ["near_only", *[x for x in CASES if x != "near_only"]],
                  "event_scenarios_included": False}
    metrics = {"status": "preregistered_not_run", "results": None, "quality_pass": None,
               "all_4_cells_all_7_scenarios_required": True,
               "no_dropping_failing_or_worse_rows": True, "no_result_driven_parameter_selection": True,
               "lag": {"method": "one near-only normalized correlation calibration per cell, then frozen",
                       "window_ms": [3000, 8000], "search_samples": [0, 320], "minimum_correlation": 0.5,
                       "at_maximum_search_boundary": "invalid; no enlarged search",
                       "always_keep_zero_lag_metrics": True},
               "far_only": {"metric": "10*log10(sum(known_echo^2)/sum(output^2))",
                            "windows_ms": {"echo_only": [[0, 40000], [3000, 40000]],
                                           "double_talk_prelude_only": [[0, 5500], [3000, 5500]]}},
               "near_fidelity": {"target": "known near component", "baseline": "unprocessed microphone mixture",
                                 "windows_ms": {"near_only": [3000, 39980], "double_talk": [6500, 39980]},
                                 "fields": ["sdr_db", "uncentered_si_sdr_db", "projection_gain_linear", "projection_gain_db",
                                            "rms_gain_db", "target_rms", "output_rms", "error_rms", "projection_residual_rms"]},
               "numeric_degeneracy": "power_floor=1e-16; explicit null/reason for silent target/output or degenerate residual; no epsilon scores",
               "timing": {"paced": False, "unit": "microseconds per 10ms render+capture pair",
                          "fields": ["p50", "p95", "p99", "max", "mean", "pairs_over_10ms"],
                          "initialization_and_reset_separate": True, "concurrent_load_record_required": True},
               "interpretation": "Explain HPF and near-waveform effects. No quality winner, production enablement, or physical acceptance inferred."}
    write_json(output / "metrics-plan.json", metrics)
    write_json(output / "input-provenance.json", {
        "source_type": "synthetic_only", "original_audio_modified": False,
        "sapi_manifest_sha256": digest(sapi_manifest.read_bytes()), "qwen_report_sha256": digest(qwen_report.read_bytes()),
        "qwen_deploy_config_sha256": qwen["deploy_config_sha256"],
        "prior_resampling_manifest_sha256": digest(prior_resampled.read_bytes()),
        "resampling": {"python": platform.python_version(), "numpy": np.__version__, "scipy": scipy.__version__,
                       "algorithm": "scipy.signal.resample_poly", "up": 2, "down": 3, "window": ["kaiser", 5.0],
                       "padtype": "constant", "cval": 0.0, "input_dtype": "float32 pcm_i16/32768",
                       "quantization": "np.rint(output*32768), clip[-32768,32767], little-endian int16",
                       "output_samples": "ceil(input_samples*2/3)", "trim": False, "denoise": False, "normalization": False,
                       "endpoint_padding": "zero outside both input boundaries; centered polyphase filter removes its own group delay",
                       "existing_method_script_sha256": digest(method_script.read_bytes())},
        "sapi": sapi_records, "qwen_near": qwen_records})
    source_map = {row["id"]: row for row in sapi_records}
    qwen_map = {row["id"]: row for row in qwen_records}
    cells = []
    for corpus in ["sapi_near", "qwen_near"]:
        for high_pass in [False, True]:
            name = f"{corpus}_hpf_{'on' if high_pass else 'off'}"
            near_map = source_map if corpus == "sapi_near" else qwen_map
            manifest = {"protocol_version": 1, "cell_id": name, "status": "frozen_not_executed",
                        "far_sources": [{key: source_map[x][key] for key in ["id", "wav", "sha256"]} for x in FAR_IDS],
                        "near_sources": [{key: near_map[x][key] for key in ["id", "wav", "sha256"]} for x in NEAR_IDS],
                        "audio_paths_relative_to": "preregistration_bundle_root",
                        "algorithm": {"crate": "sonora", "version": "0.2.0", "transparent_mode": "Legacy",
                                      "enforce_high_pass_filtering": high_pass, "noise_suppression": False, "agc": False,
                                      "other_settings": "published defaults; no filter/suppressor retuning"},
                        "parameters": parameters, "expected_input_signals": mixtures[corpus],
                        "metrics_plan": "metrics-plan.json", "results": None,
                        "human_speech": False, "independent_new_text_heldout": False,
                        "waveform_interpretation": "original SAPI baseline" if corpus == "sapi_near" else "AEC-unseen previously generated synthetic waveform; original task texts reused"}
            path = output / "cells" / (name + ".json")
            write_json(path, manifest)
            cells.append({"cell_id": name, "manifest": path.relative_to(output).as_posix(), "sha256": digest(path.read_bytes())})
    for path, expected in source_hashes:
        if digest(path.read_bytes()) != expected:
            raise ValueError("An original source changed during preparation")
    write_json(output / "protocol.json", {
        "protocol_version": 1, "status": "frozen_not_executed", "design": "2x2 fixed near waveform family by fixed high-pass setting",
        "all_cells": cells, "cell_execution_order": [row["cell_id"] for row in cells], "total_scenario_results_required": 28,
        "parameters": parameters, "metrics_plan": "metrics-plan.json", "provenance": "input-provenance.json",
        "baseline_seven_hash_bundle_sha256": baseline_hash,
        "baseline_report_sha256": digest(original_report.read_bytes()),
        "baseline_source_hashes": {name: digest((root / name).read_bytes()) for name in
            ["rust/ai-tutor/src/bin/aec_probe/fixture.rs", "rust/ai-tutor/src/bin/aec_probe/metrics.rs", "rust/ai-tutor/src/audio/echo_cancel.rs"]},
        "preparation_script_sha256": digest(Path(__file__).read_bytes()),
        "observed_original_result_retained": {"double_talk_6db_si_sdr_baseline_db": 6.199494229114549,
                                              "double_talk_6db_si_sdr_aec_db": 5.801831284826385,
                                              "projection_gain_db_ser_minus6_0_plus6": [-3.9883464017131507, -3.2366651794065286, -2.5310735168010674]},
        "source_hashes_rechecked_after_preparation": True, "aec_executed": False, "model_inference_called": False,
        "devices_used": False, "rust_or_cargo_changed": False,
        "limitations": ["Adult synthetic voices, not real learners or room acoustics",
                        "Qwen waveforms are new only to this AEC comparison; the task texts are reused",
                        "All cells and SER/delay conditions must remain visible, including worse or failed results",
                        "Original ordinary-seven input hashes and first/second reports are not overwritten",
                        "This protocol does not authorize production AEC, voice interruption or model/device use"]})
    return {"prepared": True, "cells": len(cells), "planned_results": 28,
            "qwen_resampled_files": len(qwen_records), "original_seven_hashes_unchanged": True,
            "aec_executed": False, "model_inference_called": False, "devices_used": False}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    print(json.dumps(prepare(args.root.resolve(), args.output.resolve())))
