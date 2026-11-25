"""Own Qwen for one 1800-second same-Workstation camera and synthetic voice soak.

The explicitly selected saved camera is used without target selection or PTZ.
Input is a paced fixed adult synthetic WAV; native output is requested. No
physical microphone, browser, acoustic acceptance, or automatic retry.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import queue
import secrets
import shutil
import subprocess
import threading
import time
import uuid

from camera_stream_smoke import ASSETS, PROTECTED as CAMERA_PROTECTED
from voice_loop_soak_resources import ResourceSampler
from voice_model_chain_smoke import (
    BUNDLE, GRAPH_SHA, MODEL_REVISION, PROTECTED, WARM, ProbeFailure,
    digest, require_running, save, stderr_reader, stdout_reader,
    stop_wrapper, wait_ready, warm, wsl_path,
)

TEST = 'server::voice_loop_soak_diagnostic::real_synthetic_voice_loop_camera_soak'
CHILD_BUDGET_SECONDS = 2220
DURATION_MS = 1800000
REQUEST_LIMIT = 96
CAMERA_SAMPLE_GAP_MS = 3000
PROMPTS = ('prompts/tutor_core.md', 'prompts/tutor-agent.md')


def protected(root):
    return {name: digest(root / name) if (root / name).is_file() else None
            for name in sorted(set(PROTECTED) | set(CAMERA_PROTECTED) | set(PROMPTS))}


def selected_camera(root, expected_sha256):
    value = (root / 'runtime/rust_camera_device.txt').read_text(encoding='utf-8').strip()
    actual = hashlib.sha256(value.encode('utf-8')).hexdigest()
    if not value or actual != expected_sha256:
        raise ProbeFailure('explicit_saved_camera_identity_mismatch')
    return value


def stage(root, output, executable, camera_worker, camera_sha):
    previous_root = root / 'runtime/goal-baselines/20261104/main-managed-qwen-first'
    previous = json.loads((previous_root / 'report.json').read_text(encoding='utf-8'))
    if previous.get('success') is not True or previous.get('main_exit_code') != 0:
        raise ProbeFailure('previous_qwen_fixture_not_successful')
    original = previous_root / 'fixture'
    if (original / 'runtime/local-speech-policy.json').exists():
        raise ProbeFailure('previous_fixture_policy_not_cleaned')
    fixture = output / 'tts-fixture'
    bundle = fixture / 'bin/local-speech'
    bundle.mkdir(parents=True)
    hashes = {}
    for name in BUNDLE:
        source = root / 'scripts/speech-lab' / name
        if digest(original / 'bin/local-speech' / name) != previous['bundle_sha256'][name]:
            raise ProbeFailure('frozen_qwen_bundle_changed')
        shutil.copyfile(source, bundle / name)
        hashes[name] = digest(source)
        if hashes[name] != digest(bundle / name):
            raise ProbeFailure('staged_qwen_bundle_changed')
    if hashes['qwen3_tts_12gb.yaml'] != GRAPH_SHA:
        raise ProbeFailure('qwen_graph_changed')
    links = previous['model_files_hard_linked']
    for relative in links:
        source = original / 'models' / relative
        if not source.samefile(root / 'models' / relative):
            raise ProbeFailure('model_hardlink_identity_changed')
        target = fixture / 'models' / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        os.link(source, target)
    model = fixture / 'models/speech/Qwen3-TTS-12Hz-0.6B-CustomVoice'
    manifest = json.loads((model / 'download-manifest.json').read_text(encoding='utf-8'))
    if manifest.get('revision') != MODEL_REVISION:
        raise ProbeFailure('model_revision_changed')
    for row in manifest['files']:
        if digest(model / row['file']) != row['sha256']:
            raise ProbeFailure('model_file_changed')
    relative = 'runtime/speech-gpu/omni-env-path.txt'
    (fixture / relative).parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(original / relative, fixture / relative)
    for name, expected in ASSETS.items():
        if digest(root / name) != expected:
            raise ProbeFailure('fixed_camera_asset_changed')
    source_names = [
        'rust/ai-tutor/src/voice_loop_soak_diagnostic.rs',
        'rust/ai-tutor/src/voice_loop_model_diagnostic.rs',
        'rust/ai-tutor/src/audio/voice_loop_source.rs', 'rust/ai-tutor/src/audio.rs',
        'rust/ai-tutor/src/audio/input_devices.rs', 'rust/ai-tutor/src/audio/capture_endpoint.rs',
        'rust/ai-tutor/src/audio/voice_endpoint.rs', 'rust/ai-tutor/src/voice_loop.rs',
        'rust/ai-tutor/src/recording.rs', 'rust/ai-tutor/src/segment_speech.rs',
        'rust/ai-tutor/src/vision.rs', 'rust/ai-tutor/src/vision_test_worker.rs',
        'rust/ai-tutor/src/context.rs', 'rust/ai-tutor/src/tutor.rs',
        'rust/ai-tutor/src/tutor/citation.rs', 'rust/ai-tutor/src/tutor_segments.rs',
        'rust/ai-tutor/src/tutor_delivery.rs', 'rust/ai-tutor/src/kimi.rs',
        'rust/ai-tutor/src/kimi_acp_common.rs', 'rust/ai-tutor/src/kimi_acp_stream.rs',
        'rust/ai-tutor/src/store.rs', 'rust/ai-tutor/src/server.rs',
        'scripts/validation/voice_model_chain_smoke.py', 'scripts/validation/camera_stream_smoke.py',
        'scripts/validation/voice_loop_soak_resources.py',
        'scripts/speech-lab/probe_managed_tts.py', *PROMPTS,
        'implementation-package/AI_Tutor_Codex_Implementation_Package/schemas/tutor_reply.schema.json',
    ]
    # Include split modules of the diagnostic if the implementation uses them.
    source_names.extend(p.relative_to(root).as_posix()
                        for p in sorted((root / 'rust/ai-tutor/src').glob('voice_loop_soak_*.rs'))
                        if p.relative_to(root).as_posix() not in source_names)
    method = {
        'method': 'same-workstation-camera-synthetic-voice-soak-v1-20261104', 'test': TEST,
        'diagnostics_revision': 2,
        'test_executable_sha256': digest(executable), 'camera_worker_sha256': digest(camera_worker),
        'camera_id_sha256': camera_sha, 'runner_sha256': digest(Path(__file__)),
        'source_sha256': {name: digest(root / name) for name in source_names},
        'source_hash_scope': 'Current source files and runtime prompts; executable hashes identify actual binaries. Source-to-build provenance requires the separately archived build manifest.',
        'provider_diagnostics': 'Each round records only last_acp_segments matching its accepted request_id and generation. Missing or other-call metrics remain unobserved; full snapshots before and after cleanup remain separate.',
        'camera_assets_sha256': ASSETS, 'language': 'zh',
        'duration_target_ms': DURATION_MS, 'teaching_request_limit': REQUEST_LIMIT,
        'inflight_finish_budget_seconds': 180, 'child_budget_seconds': CHILD_BUDGET_SECONDS,
        'automatic_retry': False, 'measurement_starts_after': 'camera first valid frame, ASR/TTS ready and loop armed',
        'load': 'Immediate successive production voice rounds after actual rearm; no idle padding',
        'memory_background': 'not_exercised', 'bundle_sha256': hashes,
        'resources': 'Windows child tree every5s, aggregate GPU every30s across child lifetime; no claim of exclusive WSL RAM/GPU attribution',
        'maximum_camera_observation_gap_ms': CAMERA_SAMPLE_GAP_MS,
        'camera_observation_gap_scope': 'Sampling continuity requirement, not a camera latency performance target',
        'model_revision': MODEL_REVISION,
        'model_manifest_sha256': digest(model / 'download-manifest.json'),
        'model_files_verified': len(manifest['files']), 'model_files_hard_linked': len(links),
        'environment_pointer_sha256': digest(fixture / relative),
        'previous_fixture_report_sha256': digest(previous_root / 'report.json'),
        'warmup': [{'language': lang, 'text': text, 'seed': seed} for lang, text, seed in WARM],
        'readiness_budget_seconds': 240, 'warmup_budget_per_language_seconds': 120,
        'token_transport': 'Private wrapper stdin and child environment; redacted logs',
        'physical_microphone': False, 'native_output_requested': True,
        'speaker_output_measured': False, 'simulated_display_ack': True,
        'raw_media_saved': False, 'ptz_enabled': False,
        'production_descriptor_or_policy_written': False,
        'scope': 'Same Workstation camera processing and actual ASR/teaching/native playback/rearm. Fixed synthetic voice input; no browser, acoustic, identity tracking or physical PTZ acceptance.',
    }
    with (output / 'method.json').open('x', encoding='utf-8') as stream:
        json.dump(method, stream, ensure_ascii=False, indent=2)
        stream.write('\n')
    return fixture


def check_soak(value):
    if value.get('test_type') != 'real_synthetic_voice_loop_camera_soak' or value.get('passed') is not True:
        raise ProbeFailure('soak_report_identity_or_pass_failed')
    if (value.get('language') != 'zh' or value.get('duration_target_ms') != DURATION_MS
            or value.get('teaching_request_limit') != REQUEST_LIMIT
            or value.get('budget_exhausted_before_duration') is not False
            or value.get('active_duration_ms', 0) < DURATION_MS):
        raise ProbeFailure('soak_duration_or_budget_contract_failed')
    for key in ('production_capture_loop', 'synthetic_capture_source', 'production_audio_owner',
                'simulated_display_ack', 'protected_files_unchanged', 'provider_cleanup_confirmed',
                'camera_opened', 'metadata_only'):
        if value.get(key) is not True:
            raise ProbeFailure('soak_required_proof_missing_' + key)
    for key in ('microphone_opened', 'ptz_enabled', 'raw_pcm_saved', 'speaker_output_measured',
                'browser_tested', 'acoustic_loop_tested'):
        if value.get(key) is not False:
            raise ProbeFailure('soak_scope_mismatch_' + key)
    if value.get('memory_background') != 'not_exercised':
        raise ProbeFailure('soak_memory_scope_changed')
    owned = value.get('owned_asr_stop', {})
    if any(owned.get(key) is not True for key in ('cleanup_confirmed', 'child_exited', 'interpreter_exited',
                                                'launcher_job_empty', 'interpreter_job_empty', 'interpreter_job_owned')):
        raise ProbeFailure('soak_asr_cleanup_unconfirmed')
    camera = value.get('owned_camera_stop', {})
    if camera.get('cleanup_confirmed') is not True or camera.get('supervisor_exited') is not True:
        raise ProbeFailure('soak_camera_cleanup_unconfirmed')
    if (camera.get('child_started_count') != 1 or camera.get('child_wait_confirmed_count') != 1
            or camera.get('jobs_empty_count') != 1):
        raise ProbeFailure('soak_camera_worker_restarted_or_not_reaped')
    rounds = value.get('rounds', [])
    completed = value.get('completed_rounds')
    if type(completed) is not int or not 2 <= completed <= REQUEST_LIMIT or len(rounds) != completed:
        raise ProbeFailure('soak_completed_round_count_changed')
    if any(row.get('completed') is not True or row.get('rearm_observed') is not True for row in rounds):
        raise ProbeFailure('soak_playback_or_rearm_incomplete')
    for row in rounds:
        submit = row.get('submit', {})
        bound = row.get('provider_diagnostics', {})
        acp = bound.get('last_acp_segments') or {}
        rid, generation = submit.get('request_id'), submit.get('generation')
        if (not isinstance(rid, str) or not rid or type(generation) is not int or generation <= 0
                or bound.get('matched') is not True
                or bound.get('request_id') != rid or bound.get('generation') != generation
                or acp.get('request_id') != rid or acp.get('generation') != generation
                or acp.get('success') is not True):
            raise ProbeFailure('soak_round_provider_diagnostics_unbound')
    metrics = value.get('provider_metrics', {})
    if (value.get('diagnostics_revision') != 2
            or not isinstance(value.get('provider_metrics_before_cleanup'), dict)
            or metrics != value.get('provider_metrics_after_cleanup')):
        raise ProbeFailure('soak_provider_cleanup_snapshots_missing')
    if metrics.get('requests') != completed or metrics.get('acp_segment_requests') != completed:
        raise ProbeFailure('soak_model_call_count_changed')
    lifecycle = value.get('source_lifecycle', {})
    if lifecycle.get('opens') != completed or lifecycle.get('closes') != completed or lifecycle.get('active') is not False:
        raise ProbeFailure('soak_capture_lifecycle_incomplete')
    windows = value.get('five_minute_windows', [])
    if len(windows) != 6:
        raise ProbeFailure('soak_window_evidence_missing')
    for index, window in enumerate(windows):
        if (window.get('index') != index + 1 or window.get('start_ms') != index * 300000
                or window.get('end_ms') != (index + 1) * 300000
                or window.get('covered_ms') != 300000
                or window.get('rounds_completed', 0) <= 0
                or window.get('accepted_processed_frames_delta', 0) <= 0
                or window.get('sampled_observations', 0) <= 0):
            raise ProbeFailure('soak_window_not_covered_by_real_work')
    camera = value.get('camera', {})
    if (camera.get('metadata_only') is not True or camera.get('epoch_changes') != 0
            or camera.get('samples_count', 0) <= 0 or camera.get('accepted_processed_frames', 0) <= 0):
        raise ProbeFailure('soak_camera_continuity_evidence_missing')
    if (camera.get('maximum_allowed_sample_gap_ms') != CAMERA_SAMPLE_GAP_MS
            or camera.get('observation_coverage_complete') is not True):
        raise ProbeFailure('soak_camera_observation_coverage_unconfirmed')
    for key in ('maximum_sample_gap_ms', 'tail_sample_gap_ms'):
        gap = camera.get(key)
        if not isinstance(gap, (int, float)) or not 0 <= gap <= CAMERA_SAMPLE_GAP_MS:
            raise ProbeFailure('soak_camera_observation_gap_' + key)


def run_child(root, output, executable, worker, camera_id, token, process, events, report, started):
    run = 'voice-loop-soak-' + uuid.uuid4().hex + '-zh'
    path = root / 'runtime/goal-baselines/20261104' / run / 'report.json'
    env = os.environ.copy()
    env.update(AI_TUTOR_REAL_VOICE_LOOP_SOAK='1', AI_TUTOR_REAL_VOICE_CHAIN_SYNTHETIC='1',
               AI_TUTOR_VOICE_CHAIN_LANGUAGE='zh', AI_TUTOR_VOICE_CHAIN_TTS_ENGINE='qwen',
               AI_TUTOR_VOICE_CHAIN_RUN=run, AI_TUTOR_VOICE_CHAIN_TTS_TOKEN=token,
               AI_TUTOR_SOAK_CAMERA_ID=camera_id, AI_TUTOR_SOAK_CAMERA_WORKER=str(worker),
               AI_TUTOR_SOAK_CAMERA_WORKER_SHA256=digest(worker))
    child = subprocess.Popen([str(executable), TEST, '--exact', '--ignored', '--nocapture', '--test-threads=1'],
                             cwd=root, env=env, stdin=subprocess.DEVNULL,
                             stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                             creationflags=subprocess.CREATE_NO_WINDOW)
    env.pop('AI_TUTOR_VOICE_CHAIN_TTS_TOKEN')
    child_started = time.monotonic()
    result = {'run': run, 'report': str(path), 'success': False, 'exit_code': None,
              'interrupted': False, 'elapsed_ms': 0}
    report['child'] = result
    save(output / 'report.json', report)
    stdout = stderr = b''
    failure = None
    sampler = None
    try:
        sampler = ResourceSampler(child, output)
        while True:
            sampler.sample()
            try:
                stdout, stderr = child.communicate(timeout=1)
                break
            except subprocess.TimeoutExpired:
                require_running(process, events, report, started)
                result['elapsed_ms'] = (time.monotonic() - child_started) * 1000
                save(output / 'report.json', report)
                if time.monotonic() - child_started > CHILD_BUDGET_SECONDS:
                    raise ProbeFailure('soak_child_budget_exceeded_no_retry')
    except (Exception, KeyboardInterrupt) as error:
        failure = error
        result['interrupted'] = True
        if child.poll() is None:
            child.terminate()
        try:
            stdout, stderr = child.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            child.kill()
            stdout, stderr = child.communicate(timeout=5)
    finally:
        if sampler is not None:
            report['resources'] = sampler.close(output)
        for name, data in (('soak.stdout.log', stdout), ('soak.stderr.log', stderr)):
            with (output / name).open('xb') as stream:
                stream.write(data.replace(token.encode('ascii'), b'[REDACTED]'))
        result.update(exit_code=child.returncode, elapsed_ms=(time.monotonic() - child_started) * 1000)
        if path.is_file():
            result['report_sha256'] = digest(path)
        save(output / 'report.json', report)
    if failure is not None:
        raise failure
    if child.returncode != 0:
        raise ProbeFailure('soak_process_failed_no_retry')
    check_soak(json.loads(path.read_text(encoding='utf-8')))
    require_running(process, events, report, started)
    result['success'] = True


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--real-camera-local-synthetic', action='store_true', required=True)
    parser.add_argument('--root', type=Path, required=True)
    parser.add_argument('--test-executable', type=Path, required=True)
    parser.add_argument('--camera-worker', type=Path, required=True)
    parser.add_argument('--camera-id-sha256', required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    root, output = args.root.resolve(), args.output.resolve()
    executable, worker = args.test_executable.resolve(), args.camera_worker.resolve()
    if os.name != 'nt' or not executable.is_file() or not worker.is_file():
        parser.error('Requires Windows and explicitly selected prebuilt test and release worker executables')
    output.mkdir(parents=True, exist_ok=False)
    instance, token = uuid.uuid4().hex, secrets.token_urlsafe(32)
    report = {'test_type': 'owned_qwen_same_workstation_camera_voice_soak', 'success': False,
              'instance_id': instance, 'starting': False, 'ready': False, 'warm': False,
              'stopped': False, 'linux_cleanup_confirmed': False, 'bridge_terminated_only': False,
              'wrapper_events': [], 'authentication': {}, 'errors': [], 'warmup': [], 'child': None,
              'microphone_opened': False, 'native_output_requested': True, 'real_camera_requested': True,
              'speaker_output_measured': False, 'simulated_display_ack': True, 'raw_media_saved': False,
              'automatic_retry': False, 'production_descriptor_or_policy_written': False}
    process, readers, events, before = None, [], queue.Queue(), None
    started = time.monotonic()
    try:
        before = protected(root)
        report['protected_before_sha256'] = before
        camera_id = selected_camera(root, args.camera_id_sha256)
        fixture = stage(root, output, executable, worker, args.camera_id_sha256)
        report['method_sha256'] = digest(output / 'method.json')
        linux_root = wsl_path(fixture)
        process = subprocess.Popen(['wsl.exe', '-d', 'Ubuntu', '--', 'python3',
                                    linux_root + '/bin/local-speech/managed_tts.py', '--root', linux_root,
                                    '--engine', 'qwen', '--instance', instance],
                                   stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                   creationflags=subprocess.CREATE_NO_WINDOW)
        for target, params in (
                (stdout_reader, (process.stdout, events, instance, 'qwen')),
                (stderr_reader, (process.stderr, output / 'wrapper.stderr.log', token, events))):
            reader = threading.Thread(target=target, args=params, daemon=True)
            reader.start()
            readers.append(reader)
        process.stdin.write((json.dumps({'protocol_version': 1, 'bearer_token': token}) + '\n').encode())
        process.stdin.flush()
        wait_ready(process, events, report, started, 'qwen', token)
        for language, text, seed in WARM:
            warm(language, text, seed, token, process, events, report, started)
            save(output / 'report.json', report)
        report['warm'] = True
        run_child(root, output, executable, worker, camera_id, token, process, events, report, started)
    except (Exception, KeyboardInterrupt) as error:
        report['errors'].append(str(error) if isinstance(error, ProbeFailure) else type(error).__name__)
    finally:
        if process is not None:
            try:
                stop_wrapper(process, events, readers, instance, report, started)
            except (Exception, KeyboardInterrupt) as error:
                report['errors'].append('owned_tts_cleanup_' + type(error).__name__)
                report['linux_cleanup_confirmed'] = False
        try:
            after = protected(root)
            report['protected_after_sha256'] = after
            report['protected_files_unchanged'] = before is not None and before == after
        except OSError:
            report['protected_files_unchanged'] = False
            report['errors'].append('protected_hash_check_failed')
        report['total_ms'] = (time.monotonic() - started) * 1000
        report['success'] = (not report['errors'] and report['ready'] and report['warm']
                             and report['stopped'] and report['linux_cleanup_confirmed']
                             and report['protected_files_unchanged']
                             and report['child'] is not None and report['child']['success'])
        save(output / 'report.json', report)
    print(json.dumps({'success': report['success'], 'report': str(output / 'report.json'),
                      'linux_cleanup_confirmed': report['linux_cleanup_confirmed']}))
    return 0 if report['success'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
