# Sonora AEC3 offline synthetic probe — 2026-11-04

This is an offline algorithm experiment. It does not enable AEC in AudioService,
create an input/output device, call an ASR/TTS service, or enable voice interruption.
The ordinary recording/playback ownership and defaults are unchanged.

## Fixed implementation and source

- Dependency: `sonora = { version = "=0.2.0", default-features = false }`.
- Published 2026-07-29; Rust minimum 1.91; observed local compiler 1.98.1.
- Published source commit: `a024d6ef8351add55be5e8b1d6cc35f555787660`.
- Crate SHA256: `2e1ce442ab5cb392c4067b8f2cced20be4cf12e479f6dfbf44acbe9b14ac9250`.
- BSD-3-Clause. No Sonora `examples` feature; its optional CPAL 0.18 is not used.
- Real Sonora AEC3, not a project-written cancellation filter. The project FIR
  creates a known test echo only; it never removes echo from the signal.
- The default ten-case `Config` leaves NS, AGC, pre-amplification and high-pass filtering disabled.
  `EchoCanceller.enforce_high_pass_filtering` is explicitly false because its
  default would otherwise enable high-pass filtering.
- Upstream main's render-buffer headroom fix for underruns remains Unreleased;
  it is not included in the pinned version or credited in this experiment.

Primary sources: [published manifest](https://github.com/dignifiedquire/sonora/blob/a024d6ef8351add55be5e8b1d6cc35f555787660/crates/sonora/Cargo.toml),
[published configuration](https://github.com/dignifiedquire/sonora/blob/a024d6ef8351add55be5e8b1d6cc35f555787660/crates/sonora/src/config.rs),
[0.2.0 API](https://docs.rs/sonora/0.2.0/sonora/struct.AudioProcessing.html),
[upstream change log](https://github.com/dignifiedquire/sonora/blob/main/CHANGELOG.md).

## File boundaries

- `rust/ai-tutor/src/audio/echo_cancel.rs`: `EchoCanceller16k` owns one Sonora
  instance and 160-sample scratch buffer. Each pair validates normalized finite
  PCM, processes render, supplies known device delay, then processes capture.
  Reset creates a fresh instance; fixed-size frames prevent partial-frame calls.
- `rust/ai-tutor/src/bin/aec-probe.rs`: explicitly invoked offline runner. It
  privately includes the adapter by path; `audio.rs` does not declare it.
- `rust/ai-tutor/src/bin/aec_probe/fixture.rs`: fixed input provenance, bounded
  WAV reader and known synthetic mixtures.
- `rust/ai-tutor/src/bin/aec_probe/metrics.rs`: known-reference metrics, single
  control-based lag calibration, processing-time summaries.

No production audio, server, UI, database, or runtime descriptor is connected.

## Corpus and cases

Input is exactly the 12 author-written adult SAPI-to-file WAVs in
`runtime/goal-baselines/20261103/sapi-corpus/manifest.json`. The probe checks the
source labels, PCM16 mono 16 kHz format, unique IDs and hard-coded SHA256 for each
file. It reads `<id>.wav` beside the manifest, ignoring arbitrary manifest paths.
Another recording cannot silently replace a fixture by changing the manifest.
Neither transcript text, file paths nor audio samples are written to the report.

Six disjoint clips form each far/near tape, separated by 200 ms silence. Each
scenario is 40 synthetic seconds so the ordinary echo/double-talk cases traverse
all clips. Tapes repeat deterministically and keep the same phase across cases.
Far RMS is normalized to 0.10 before mixing. A single common scalar, reported
per case, keeps the render/capture peaks at or below 0.8 without clipping.

The known echo consists of taps `(offset samples, gain)` = `(0, 0.55)`,
`(192, 0.18)`, `(432, -0.08)`, after the declared device buffer delay. The declared
delay is simulated render-plus-capture device buffering; extra reflections are
the FIR offsets, not an additional fitted delay. Render is always submitted to
Sonora before that iteration's capture.

| Scenario | Change |
| --- | --- |
| Echo only, 20/80/160 ms | Three separate known device delays |
| Near only | No far signal or echo; independent lag/fidelity control |
| Double-talk, −6/0/+6 dB SER | Near speech starts at 6 seconds; RMS ratio measured against known echo over the near interval |
| Render stop | Render becomes zero at 8 seconds; delayed echo persists for up to 107 ms; AEC state is retained |
| Delay jump | Device delay changes from 80 to 160 ms at 8 seconds; declared delay is updated without resetting |
| Reference gap | One 10 ms reference frame is missing at 8 seconds; its output is excluded, then AEC is reset; no missing packet is passed off as valid silence |

The event time is fixed at 8.000 seconds using input/FIR energy only, without
consulting AEC output. Before processing those three scenarios, the probe checks
active pre-event render and the appropriate tail/jump/gap signal RMS exceed 0.01.
Stop uses the original uncut render peak for common scaling, preserving the same
prefix as the ordinary 80 ms echo case. Event sample/ms, energy windows and RMS
are recorded. Stop reports its exact 107 ms possible echo tail and the first
full second separately. The seven ordinary cases retain their original inputs.

## Metric interpretation

All input, component and output signals have hashes. The report includes source
parameters and excludes the deliberately unprocessed frame from scoring.

- ERLE is `10 log10(sum(echo²) / sum(output²))` only in near-absent windows.
  Double-talk ERLE ends 500 ms before near speech starts. The same interval is
  used after fixed output alignment. Full windows and post-3-second adaptation
  windows are separate. Stop, delay-change and reset recovery have extra windows.
- Double-talk is scored against its known near component: SDR, uncentered
  SI-SDR, projection gain, RMS gain, target/output RMS, error and projection
  residual RMS. The unprocessed microphone mixture is the baseline. Low output
  energy or high SI-SDR alone cannot establish near-speech preservation.
- The near-only control runs first, once. Positive output lags 0–320 samples
  (0–20 ms) are compared by normalized correlation in its fixed 3–8 second
  window. The best lag is frozen for every scenario. Correlation below 0.5 or
  a maximum at 320 samples makes calibration unusable; calibrated scores are
  null and zero-lag results remain. There is no per-case alignment search.
- Silent target/output, zero projection and vanishing residual produce null
  scores with explicit reasons, never epsilon-inflated perfect scores. RMS
  quantities remain available. The report does not fabricate quality PASS.
- Per-pair p50/p95/p99/max and mean cover validation, render processing, delay
  setter and capture processing. Initialization/reset are separate. Processing
  is non-paced and includes neither devices nor any ASR/TTS inference. Record
  concurrent load; these numbers are not microphone latency or audible stop time.

## Validation and invocation

Nine ordinary Rust tests cover adapter reset and invalid-input state preservation,
component/SER/FIR boundaries, malformed WAV, muted-output metrics, known 20 dB
attenuation and excluded frames, lag detection/search exhaustion, timing
percentiles, reference-gap/reset accounting, and rejection of a silent stop-tail
fixture. One additional ignored test requires the generated fixed SAPI corpus;
it verifies all three event stimuli and their unchanged pre-event prefixes.
The actual probe always checks those real-corpus event energies before AEC,
regardless of whether the optional corpus test was invoked. The implementation
agent did not run Cargo; root owns compilation and actual probe runs.

Root's first release run is retained at
`runtime/goal-baselines/20261104/aec-offline-first/report.json`: all ten cases ran
in 5373 ms; fixed near-only lag was 128 samples (8 ms), p99 pair processing was
621.2 microseconds, and no pair exceeded 10 ms. These are offline processing
measurements, not end-to-end or physical performance.

The first fixture placed events at 9 seconds, which was silent in the source:
its stop-tail echo RMS was zero and cannot validate tail cancellation. Its first
delay-jump second was also weak (~0.00215 RMS). Those measurements are preserved
as an invalid event stimulus, not retrospectively labelled PASS. The revised
8-second input-only checks give approximately 0.05067 RMS for the stop's known
107 ms tail, 0.04609 for the first delay-jump second and 0.04795 for the gap's
capture frame. Root must run a fresh `aec-offline-second` report to measure the
revised cases.

Root completed `aec-offline-second/report.json` after rebuilding all native bins.
All ten cases ran in 5353 ms; 39999 frame pairs had p95 553.2 microseconds,
p99 598.7 microseconds, max 921.4 microseconds and zero over 10 ms. The fixed
near-only lag remained 8 ms. `unchanged-controls.json` confirms the ordinary seven
cases have exactly the same four input hashes **and output hashes** as the first
run; the double-talk attenuation finding is unchanged.

The corrected 8-second stop has real echo RMS 0.05067 in its 107 ms known tail;
the residual RMS was 0.00001579 after the fixed lag, or ERLE 70.13 dB. The first
second after the delay jump produced only 6.22 dB ERLE; after the reference gap
and reset it was 8.66 dB, with the dropped frame explicitly excluded. Three
seconds later the corresponding windows reached 69.23/68.95 dB. These are
synthetic linear-room observations, not a claim of instant recovery, device
acceptance or safe automatic voice interruption.

The code and metrics are covered by `workspace-fifteenth.log` (539 passed,
0 failed, 3 ignored). The optional fixed-corpus test was separately executed
successfully in `aec-fixed-corpus-first.log`, so its ordinary-suite ignored
status is not represented as a failure or silently removed. Native release
evidence is `native-release-seventh.log`.

The first report also does **not** establish speech-quality acceptance: in the
+6 dB double-talk case, SI-SDR decreased from 6.20 dB unprocessed to 5.80 dB after
AEC. Near-speech projection gains were −3.99/−3.24/−2.53 dB in the −6/0/+6 dB
cases. These attenuation findings remain; neither the event correction nor a
good echo-only score resolves them.

From the workspace root, after root builds `aec-probe`:

```powershell
.\target\release\aec-probe.exe `
  --corpus .\runtime\goal-baselines\20261103\sapi-corpus\manifest.json `
  --output .\runtime\goal-baselines\20261104\aec-offline-first\report.json `
  --load-note 'Describe actual concurrent compilation, model inference and other load'
```

Use a fresh report path; existing evidence is not overwritten. No synthetic
audio is saved, no real recording is accepted, and no device consent is implied.

## Preregistered 2×2 mode

The optional `--protocol` mode accepts exactly the frozen protocol with SHA256
`74d6e6f0010f789a6bcea40e31647f883b1c53a6351174850a7535934e784b6e` in
`runtime/goal-baselines/20261104/aec-2x2-preregistered/protocol.json`. It is
mutually exclusive with `--corpus`; the original ten-case invocation, inputs and
report shape remain available. This new implementation is **not** part of the
539-test checkpoint cited above. Root subsequently ran the targeted tests and
release probe, with the evidence recorded below.

The complete design is four cells: original SAPI near / previously generated
Qwen chunk8 near, each with Legacy AEC high-pass enforcement off / on. NS and
AGC stay off, and all other settings remain the published defaults. Every cell
uses the same original six SAPI far clips. The six Qwen near WAVs use the same
task texts as the earlier synthetic corpus; they are new only to this AEC
comparison, not independent new texts or human recordings. Their separate
24→16 kHz conversion, hashes, endpoint convention and energy are frozen in
`input-provenance.json`; no resampling or inference happens in the Rust probe.

Every cell executes all seven ordinary scenarios: near-only control, three
echo-only delays (20/80/160 ms), and double-talk at −6/0/+6 dB SER. The event
scenarios are outside this preregistration and remain in the original mode.
The near-only control executes first once per cell; its 3–8 second calibration
uses the existing 0–320 sample search and is frozen for that cell. Reports keep
zero-lag and usable fixed-lag ERLE/fidelity, projection gain and residual, RMS
gain, uncentered SI-SDR (`si_sdr_db`), SDR, all component/output hashes, timing
and explicit undefined reasons. The known unfiltered near target is the same
for HPF off/on: filtering is not subtracted from the fidelity penalty.

Before creating any AEC instance, the probe verifies the protocol, four manifest
hashes, metrics plan, provenance, all WAV hashes/formats, and all four input
signal hashes for each of the 28 planned rows. A modified input or changed
mixture implementation aborts before scoring. The baseline seven-case bundle
remains `514046b5375bc9516d1815fe4c69cb8afaf586fe8feb848c8f5c50c393700033`.
The frozen source hashes describe the preparation-time implementation; the
new binary does not incorrectly require its modified source files to have
those earlier hashes.

The report uses `probe: offline_synthetic_aec3_2x2`, `cells[4].cases[7]`, the
frozen protocol/metrics plan and individual manifest hashes. Processing errors
retain a failed row and input hashes, then continue all remaining cases/cells;
the report is saved and the process exits nonzero. A failed near-only run makes
calibrated scores unavailable, without changing the control or enlarging the
search. Poor scores or an unusable calibration do not hide rows. There is no
quality PASS, winner selection or production enablement.

New regression coverage checks preserved HPF configuration across reset,
protocol tamper rejection before audio loading, changed capture hash rejection,
all failed rows/control retention, and zero-lag ERLE when calibration is unusable.
An additional ignored fixed-bundle test verifies all 28 input hashes without
running AEC or using devices. After root compiles, the intended commands are:

```powershell
cargo test -p ai-tutor --bin aec-probe
cargo test -p ai-tutor --bin aec-probe fixed_comparison_sources -- --ignored
.\target\release\aec-probe.exe `
  --protocol .\runtime\goal-baselines\20261104\aec-2x2-preregistered\protocol.json `
  --output .\runtime\goal-baselines\20261104\aec-2x2-first\report.json `
  --load-note 'Record actual concurrent CPU/GPU/model activity'
```

Keep the original first/second reports and all preregistration files unchanged.
Root should also compare the SAPI/HPF-off cell's seven input and output hashes
against `aec-offline-second/report.json`. No result from the new mode has been
observed or selected at implementation time.

### First actual 2×2 result

`aec-e5-targeted-first.log` records 14 AEC tests passed, 0 failed and 2 ignored.
The release run at `aec-2x2-first/report.json` completed all 28 planned rows in
11.198 seconds. Its 112000 processed pairs had p95 381.4 microseconds,
p99 534.6 microseconds and maximum 1690.5 microseconds; none exceeded 10 ms.
These remain non-paced algorithm measurements, not device or end-to-end latency.

`aec-2x2-first/baseline-comparison.json` archives all 28 rows and the explicit
seven-control comparison. All four input hashes **and output hashes** of every
SAPI/HPF-off ordinary case match `aec-offline-second/report.json` exactly.
The original reports and frozen inputs were not changed.

The table retains both near-only RMS/projection measures and all three
double-talk SER results. Values are fixed-lag, in dB; double-talk triplets are
ordered −6/0/+6 dB SER, with no rows selected by their outcome.

| Cell | Lag, ms | Near-only RMS gain / projection gain / SI-SDR | Double-talk SI-SDR | Double-talk projection gain |
| --- | ---: | --- | --- | --- |
| SAPI, HPF off | 8.0000 | ≈0 / ≈0 / 129.38 | 3.38 / 4.48 / 5.80 | −3.99 / −3.24 / −2.53 |
| SAPI, HPF on | 7.9375 | −0.44 / −4.46 / −1.84 | −5.37 / −4.92 / −4.33 | −9.35 / −8.53 / −7.68 |
| Qwen, HPF off | 8.0000 | ≈0 / ≈0 / 128.60 | 3.11 / 4.76 / 6.04 | −4.09 / −3.08 / −2.39 |
| Qwen, HPF on | 7.8750 | −0.24 / −5.08 / −3.10 | −5.62 / −5.26 / −4.99 | −9.33 / −8.48 / −7.89 |

All four fixed-lag calibrations were usable. HPF-on scores use the unchanged
**unfiltered** near reference: a single lag does not remove the filter's
frequency-dependent phase or magnitude response. In near-only controls the
RMS changes are only −0.44/−0.24 dB, despite much lower projection gains and
SI-SDR. Those projection scores must not all be described as actual amplitude
suppression, or used alone to claim worse perceived speech quality.

The HPF-off +6 dB double-talk result remains slightly below the unprocessed
baseline for both waveform families: SAPI 6.20→5.80 dB and Qwen 6.17→6.04 dB.
The similar HPF-off attenuation finding therefore persists with the alternative
near waveform, but these reused synthetic texts do not establish real-room or
learner speech quality. Echo-only post-adaptation ERLE is about 69.3–69.7 dB;
that does not resolve double-talk fidelity or authorize production AEC.

## Remaining production and physical acceptance

This establishes an offline algorithm path only. Later integration must tap the
actual PCM callback after format/rate/any future speed conversion, including
underrun/cancel silence; preserve input/output timestamps; split 10 ms frames
on a bounded worker; and handle discontinuity and device changes explicitly.
The existing input callback currently discards timestamps, and capture/playback
are mutually exclusive. Neither fact is changed here.

Stopping speech invalidates its playback epoch immediately, but learned AEC state
and submitted reference history may be needed during a bounded echo tail while
an explicitly enabled microphone remains active. It must not extend microphone
use after the user closes listening. SAPI and current non-normal-speed fallback
have no PCM reference and must not enable voice interruption. CPAL's playback
timestamp predicts device output; it is not a measured speaker receipt.

Separate physical acceptance is required for real microphone/speaker pairs,
volume/clipping, far-only false interruptions, near speech during playback,
stop-tail behavior, disconnects, and long-term independent-device clock drift.
Synthetic adult speech and a short linear FIR establish none of those results.
