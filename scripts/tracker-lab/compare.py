"""Fixed synthetic detections -> official OC-SORT oracle -> shared Rust policy.
No cameras, model weights, videos, identities or tracking service are accessed.
"""
import argparse
import hashlib
import json
import math
from pathlib import Path
import sys
import time
import types

ROOT = Path(__file__).resolve().parents[2]
VENDOR = ROOT / "vendor/ocsort-reference"


def dump_rows(path, rows):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("".join(json.dumps(row, ensure_ascii=False, separators=(",", ":")) + "\n" for row in rows), encoding="utf-8")


def read_rows(path):
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line]


def face(identity, x, y=0.2, score=0.9):
    return {"truth_id": identity, "xyxy": [x * 1280, y * 720, (x + .12) * 1280, (y + .18) * 720], "score": score}


def generate(path):
    rows = []

    def add(name, frames, recovery_ms=None):
        for index, (at, detections) in enumerate(frames):
            # Input permutations are deterministic and shared by both trackers.
            if index % 2:
                detections = list(reversed(detections))
            row = {"case_id": name, "epoch": name, "frame_id": index + 1, "timestamp_ms": at,
                   "frame_age_ms": 0, "width": 1280, "height": 720, "detections": detections,
                   "target_truth": "A", "recovery_at_ms": recovery_ms}
            if index == 4:
                row["select_truth"] = "A"
            rows.append(row)

    for people in (1, 2, 3):
        frames = []
        for index in range(75):
            offset = 0 if index < 5 else .025 * math.sin((index - 5) / 8)
            frames.append((index * 33, [face(chr(65 + person), .1 + person * .3 + offset) for person in range(people)]))
        add(f"steady_{people}", frames)
    add("unrelated_cross", [(i * 33, [face("A", .1), face("B", .5 + max(i - 5, 0) * .005), face("C", .8 - max(i - 5, 0) * .005)]) for i in range(66)])
    add("selected_cross", [(i * 33, [face("A", .2 + max(i - 5, 0) * .006), face("B", .7 - max(i - 5, 0) * .006)]) for i in range(66)])
    for lost_ms in (100, 300, 500, 600, 601, 800):
        frames = [(i * 33, [face("A", .2), face("B", .7)]) for i in range(5)]
        recovery = 132 + lost_ms
        frames += [(at, [face("B", .7)]) for at in range(165, recovery, 33)]
        frames += [(recovery + i * 33, [face("A", .21), face("B", .7)]) for i in range(15)]
        add(f"occlusion_{lost_ms}ms", frames, recovery)
    add("nonlinear", [(i * 33, [face("A", .3 + (0 if i < 5 else .06 * math.sin(i / 12)), .2 + (0 if i < 5 else .035 * math.sin(i / 9)))]) for i in range(100)])
    add("confidence_dip", [(i * 33, [face("A", .2 + .0005 * i, score=(.2 if 12 <= i <= 18 else .4 if i == 19 else .9)), face("B", .7)]) for i in range(55)])
    times = [0, 33, 66, 99, 132, 165, 198, 560, 593, 626, 1250, 1283, 1316, 1349, 1382, 1415]
    add("irregular_frames", [(at, [face("A", .2 + .01 * math.sin(at / 200)), face("B", .7)]) for at in times])
    for fps in (10, 15):
        add(f"nonlinear_{fps}hz", [(round(i * 1000 / fps), [face("A", .3 + (0 if i < 5 else .04 * math.sin(i / 6)))]) for i in range(50)])
    dump_rows(path, rows)
    print(json.dumps({"fixture": str(path), "frames": len(rows), "cases": len(set(row["case_id"] for row in rows)), "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}))


def load_reference():
    provenance = json.loads((VENDOR / "provenance.json").read_text(encoding="utf-8"))
    for item in provenance["files"]:
        assert hashlib.sha256((VENDOR / item["path"]).read_bytes()).hexdigest() == item["sha256"]
    sys.path.insert(0, str(VENDOR))
    source = (VENDOR / "trackers/ocsort_tracker/ocsort.py").read_text(encoding="utf-8")
    # Observational instrumentation only: attach exact input indices at the
    # existing assignment statements. Do not infer matches with another IOU.
    prefix, suffix = source.split("    def update_public(", 1)
    edits = [
        ("self.frame_count += 1\n        # post_process detections", "self.frame_count += 1\n        self.frame_assignments = {}\n        # post_process detections"),
        ("dets_second = dets[inds_second]", "indices_second = np.where(inds_second)[0]\n        dets_second = dets[inds_second]"),
        ("dets = dets[remain_inds]", "indices_main = np.where(remain_inds)[0]\n        dets = dets[remain_inds]"),
        ("self.trackers[m[1]].update(dets[m[0], :])", "self.trackers[m[1]].update(dets[m[0], :])\n            self.frame_assignments[int(indices_main[m[0]])] = self.trackers[m[1]]"),
        ("self.trackers[trk_ind].update(dets_second[det_ind, :])", "self.trackers[trk_ind].update(dets_second[det_ind, :])\n                    self.frame_assignments[int(indices_second[det_ind])] = self.trackers[trk_ind]"),
        ("self.trackers[trk_ind].update(dets[det_ind, :])", "self.trackers[trk_ind].update(dets[det_ind, :])\n                    self.frame_assignments[int(indices_main[det_ind])] = self.trackers[trk_ind]"),
        ("self.trackers.append(trk)", "self.trackers.append(trk)\n            self.frame_assignments[int(indices_main[i])] = trk"),
    ]
    for old, new in edits:
        assert prefix.count(old) == 1, f"Pinned reference trace patch mismatch: {old}"
        prefix = prefix.replace(old, new)
    traced = prefix + "    def update_public(" + suffix
    module = types.ModuleType("trackers.ocsort_tracker.ocsort_traced")
    module.__package__ = "trackers.ocsort_tracker"
    exec(compile(traced, "official-ocsort-with-assignment-trace", "exec"), module.__dict__)
    return module, provenance, hashlib.sha256(traced.encode()).hexdigest()


def oracle(fixture, output, use_byte):
    import numpy as np
    module, provenance, traced_hash = load_reference()
    rows = read_rows(fixture)
    fixture_hash = hashlib.sha256(fixture.read_bytes()).hexdigest()
    frames, timings = [], []
    params = dict(det_thresh=.4, max_age=18, min_hits=3, iou_threshold=.25, delta_t=3, inertia=.2, use_byte=use_byte)
    for repeat in range(20):
        current = None
        for row in rows:
            if row["case_id"] != current:
                module.KalmanBoxTracker.count = 0
                tracker = module.OCSort(**params)
                current = row["case_id"]
            detections = np.array([det["xyxy"] + [det["score"]] for det in row["detections"]], dtype=np.float64).reshape((-1, 5))
            started = time.perf_counter_ns()
            emitted = tracker.update(detections, (row["height"], row["width"]), (row["height"], row["width"]))
            elapsed = (time.perf_counter_ns() - started) / 1e6
            timings.append(elapsed)
            if repeat == 0:
                assignments = []
                for index in range(len(detections)):
                    track = tracker.frame_assignments.get(index)
                    assignments.append(None if track is None else {"track_id": int(track.id + 1), "count": int(track.hits + 1), "native_emitted": int(track.id + 1) in set(emitted[:, 4].astype(int))})
                frames.append({"fixture_sha256": fixture_hash, "case_id": row["case_id"], "frame_id": row["frame_id"], "assignments": assignments, "update_ms": elapsed})
    dump_rows(output, frames)
    metadata = {"fixture_sha256": fixture_hash, "source_commit": provenance["commit"], "trace_source_sha256": traced_hash, "params": params,
                "repeats": 20, "assignment_solver": "scipy.optimize.linear_sum_assignment (LAP not installed)",
                "update_p50_ms": float(np.percentile(timings, 50)), "update_p95_ms": float(np.percentile(timings, 95)), "update_p99_ms": float(np.percentile(timings, 99)),
                "includes_trace_instrumentation": True, "coordinates": "pixel XYXY", "devices_opened": False,
                "adapter": "Original association with per-detection trace; downstream Rust uses its existing cumulative 3-observation confirmation, not OC-SORT initial-frame/hit_streak emission exceptions"}
    output.with_suffix(".timings.json").write_text(json.dumps(metadata, indent=2), encoding="utf-8")
    print(json.dumps(metadata))


def verify(fixture):
    import numpy as np
    traced, _, _ = load_reference()
    from trackers.ocsort_tracker import ocsort as original
    rows = read_rows(fixture)
    checked = 0
    for use_byte in (False, True):
        current = None
        for row in rows:
            if current != row["case_id"]:
                original.KalmanBoxTracker.count = traced.KalmanBoxTracker.count = 0
                params = dict(det_thresh=.4, max_age=18, min_hits=3, iou_threshold=.25, delta_t=3, inertia=.2, use_byte=use_byte)
                expected, actual = original.OCSort(**params), traced.OCSort(**params)
                current = row["case_id"]
            detections = np.array([det["xyxy"] + [det["score"]] for det in row["detections"]], dtype=np.float64).reshape((-1, 5))
            size = (row["height"], row["width"])
            assert np.array_equal(expected.update(detections.copy(), size, size), actual.update(detections.copy(), size, size))
            assert len(expected.trackers) == len(actual.trackers)
            for left, right in zip(expected.trackers, actual.trackers):
                assert left.id == right.id and np.array_equal(left.kf.x, right.kf.x) and np.array_equal(left.kf.P, right.kf.P)
            ids = [track.id for track in actual.frame_assignments.values()]
            assert len(ids) == len(set(ids))
            for index, track in actual.frame_assignments.items():
                observation = track.last_observation if track.last_observation.sum() >= 0 else track.get_state()[0]
                assert np.allclose(detections[index, :4], observation[:4])
            checked += 1
    print(json.dumps({"trace_patch_parity": "PASS", "frames_checked": checked, "checks": ["native public output", "track ids", "Kalman x/P", "exact assignment input index"], "devices_opened": False}))


def report(fixture, paths, output):
    source = read_rows(fixture)
    expected_hash = hashlib.sha256(fixture.read_bytes()).hexdigest()
    results = []
    for path in paths:
        trace = read_rows(path)
        assert len(trace) == len(source)
        by_case = {}
        for row, result in zip(source, trace):
            assert result["fixture_sha256"] == expected_hash and (row["case_id"], row["frame_id"]) == (result["case_id"], result["frame_id"])
            current = by_case.setdefault(row["case_id"], {"id_switches": 0, "wrong_selected_frames": 0, "selected_visible_frames": 0,
                "target_visible_frames_after_selection": 0, "selection_clears": 0, "initial_target_id": None,
                "recovery_same_id": None, "recovery_delay_ms": None, "policy_recovery_delay_ms": None, "last_ids": {}, "last_selected": None, "selected_started": False})
            if row.get("select_truth"):
                current["selected_started"] = True
            for detection, assignment in zip(row["detections"], result["assignments"]):
                truth = detection["truth_id"]
                if assignment is None:
                    continue
                identity = assignment["track_id"]
                old = current["last_ids"].get(truth)
                if old is not None and old != identity:
                    current["id_switches"] += 1
                current["last_ids"][truth] = identity
                if row.get("select_truth") == truth:
                    current["initial_target_id"] = identity
                if truth == "A" and row.get("recovery_at_ms") is not None and row["timestamp_ms"] >= row["recovery_at_ms"] and current["recovery_same_id"] is None:
                    current["recovery_same_id"] = identity == current["initial_target_id"]
                    current["recovery_delay_ms"] = row["timestamp_ms"] - row["recovery_at_ms"]
            if current["selected_started"]:
                if row.get("recovery_at_ms") is not None and row["timestamp_ms"] >= row["recovery_at_ms"] and result["selected_truth"] == "A" and result["selected_status"] == "visible" and current["policy_recovery_delay_ms"] is None:
                    current["policy_recovery_delay_ms"] = row["timestamp_ms"] - row["recovery_at_ms"]
                current["target_visible_frames_after_selection"] += any(det["truth_id"] == "A" for det in row["detections"])
                current["selected_visible_frames"] += result["selected_truth"] == "A" and result["selected_status"] == "visible"
                current["wrong_selected_frames"] += result["selected_truth"] not in (None, "A")
                current["selection_clears"] += current["last_selected"] is not None and result["selected_key"] is None
            current["last_selected"] = result["selected_key"]
        cases = []
        for name, value in by_case.items():
            for key in ("last_ids", "last_selected", "selected_started"):
                value.pop(key)
            value["case_id"] = name
            value["selected_visible_fraction"] = value["selected_visible_frames"] / max(1, value["target_visible_frames_after_selection"])
            cases.append(value)
        results.append({"trace": str(path), "cases": cases, "total_id_switches": sum(row["id_switches"] for row in cases), "total_wrong_selected_frames": sum(row["wrong_selected_frames"] for row in cases)})
    report = {"fixture_sha256": expected_hash, "synthetic_only": True, "real_camera_accuracy_verified": False,
              "policy": "Both pass through the same production Rust selected-target State reducer; no tracker switch", "results": results}
    output.write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
    print(json.dumps({"report": str(output), "summary": [{key: row[key] for key in ("trace", "total_id_switches", "total_wrong_selected_frames")} for row in results]}))


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="action", required=True)
    generate_parser = sub.add_parser("generate")
    generate_parser.add_argument("--output", type=Path, required=True)
    oracle_parser = sub.add_parser("oracle")
    oracle_parser.add_argument("--fixture", type=Path, required=True)
    oracle_parser.add_argument("--output", type=Path, required=True)
    oracle_parser.add_argument("--use-byte", action="store_true")
    verify_parser = sub.add_parser("verify")
    verify_parser.add_argument("--fixture", type=Path, required=True)
    report_parser = sub.add_parser("report")
    report_parser.add_argument("--fixture", type=Path, required=True)
    report_parser.add_argument("--trace", type=Path, action="append", required=True)
    report_parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.action == "generate": generate(args.output)
    elif args.action == "oracle": oracle(args.fixture, args.output, args.use_byte)
    elif args.action == "verify": verify(args.fixture)
    else: report(args.fixture, args.trace, args.output)
