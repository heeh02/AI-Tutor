# Same-detection tracker comparison

No device, video, learned model or production tracker is opened/switched. The fixture is synthetic and explicitly labels temporary ground-truth persons A/B/C. It does not prove real camera accuracy.

Reference: `vendor/ocsort-reference/provenance.json` pins upstream commit, source URLs and local SHA256 for the unmodified OC-SORT core and MIT license. `compare.py` checks those hashes, applies an in-memory assignment-index trace only, and `verify` compares it against the original module. The trace does not replace the algorithm with IOU matching.

The isolated environment is `runtime/tracker-lab/.venv` (Python 3.10). Reproduction installation is `python -m pip install --no-deps -r scripts/tracker-lab/requirements.txt`; FilterPy's unused plotting dependency is intentionally not installed. NumPy 2.2.6, SciPy 1.15.3 and FilterPy 1.4.5 were imported successfully. LAP is absent, so the official SciPy assignment fallback is used. Do not install these into speech-lab or a GPU model environment.

From the workspace root:

```powershell
$py = 'runtime/tracker-lab/.venv/Scripts/python.exe'
$out = 'runtime/goal-baselines/20261104/tracker-compare'
& $py scripts/tracker-lab/compare.py generate --output "$out/fixture.jsonl"
& $py scripts/tracker-lab/compare.py verify --fixture "$out/fixture.jsonl"
& $py scripts/tracker-lab/compare.py oracle --fixture "$out/fixture.jsonl" --output "$out/ocsort-default.raw.jsonl"
& $py scripts/tracker-lab/compare.py oracle --fixture "$out/fixture.jsonl" --output "$out/ocsort-byte.raw.jsonl" --use-byte

# Root agent builds this binary together with the workspace. No device action.
$probe = 'target/release/tracker-compare.exe'
& $probe --fixture "$out/fixture.jsonl" --output "$out/bytetrack.policy.jsonl"
& $probe --fixture "$out/fixture.jsonl" --candidate-trace "$out/ocsort-default.raw.jsonl" --output "$out/ocsort-default.policy.jsonl"
& $probe --fixture "$out/fixture.jsonl" --candidate-trace "$out/ocsort-byte.raw.jsonl" --output "$out/ocsort-byte.policy.jsonl"
& $py scripts/tracker-lab/compare.py report --fixture "$out/fixture.jsonl" --trace "$out/bytetrack.policy.jsonl" --trace "$out/ocsort-default.policy.jsonl" --trace "$out/ocsort-byte.policy.jsonl" --output "$out/report.json"
```

Current fixture: 835 frames, 16 cases, SHA256 `0a891752dbf9a2f81636a4912123d174c580b6b0e99d8bdd89c331c36b87dbe2`. Cases include 1/2/3 persons, unrelated/selected crossing, 100/300/500/600/601/800 ms occlusion, nonlinear motion, confidence dip including exact 0.4, irregular timestamps and 10/15 Hz inputs. Detection ordering alternates deterministically. Both paths read the exact same file and reject mismatched fixture hashes/frame identities.

ByteTrack runs the pinned `edgefirst-tracker=0.38.2` with production settings (high confidence 0.4, IOU 0.25, update 0.25, lifespan 600000000 ns). Input pixel coordinates are normalized exactly as in the production detector. OC-SORT uses original pixel coordinates, det_thresh 0.4, max_age 18 frames, min_hits 3, IOU 0.25, delta_t 3, inertia 0.2. Default and optional BYTE are reported separately. Frame-based max_age has not been relabeled as 600 ms when input rate changes.

The Rust adapter runs both through production `tracked_faces`, scoped ambiguity and `State` reducers. It uses the existing cumulative three-observation confirmation, while the reference raw trace records native OC-SORT emission separately. This deliberately compares association alternatives under the same application policy; it does not claim the policy adapter exactly reproduces OC-SORT's initial-frame/hit_streak publishing exceptions. Ground-truth IDs never enter either tracker.

Actual pre-build reference check: `verify` passed 1670 frame updates (835 × both BYTE settings), comparing native emitted arrays, track IDs and Kalman x/P exactly, and checking the trace's detection-index association. Initial Python timings are in each `.timings.json`; they include Python dispatch and trace instrumentation and were collected on a shared host without asserting an exclusive-load window. Repeat for a stable timing comparison after builds stop. Rust code was not compiled/run by this sub-agent; root supplies those results.

`report.json` separates raw ID switches, first raw same-ID recovery, production-policy recovery delay, wrong selected frames, selection clears and target availability. A zero wrong-selected count is insufficient if the tracker continually clears selection; inspect both safety and availability, especially the unrelated-cross case. No default production tracker switch follows automatically from this report.
