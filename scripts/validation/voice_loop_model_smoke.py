"""Owned Qwen plus real production voice-loop transitions on paced synthetic WAVs.

Only the input source and display acknowledgements are synthetic. The ignored
Rust diagnostic uses production capture/VAD/ASR, provider and native output.
This is not microphone, browser or acoustic acceptance. No automatic retry.
"""
import argparse
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

from voice_model_chain_smoke import (
    BUNDLE, GRAPH_SHA, LAB, MODEL_REVISION, WARM, ProbeFailure,
    digest, protected, require_running, save, stderr_reader, stdout_reader,
    stop_wrapper, wait_ready, warm, wsl_path,
)

TEST = 'server::voice_loop_model_diagnostic::real_synthetic_voice_loop_two_rounds'
ROUND_BUDGET_SECONDS = 480


def stage(root, output, executable):
    # Reuse the verified model assets and ownership helpers. Keep the previous
    # diagnostic and its frozen methods unchanged; this method has its own scope.
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
    source_names = [
        'rust/ai-tutor/src/voice_loop_model_diagnostic.rs',
        'rust/ai-tutor/src/audio/voice_loop_source.rs',
        'rust/ai-tutor/src/audio.rs', 'rust/ai-tutor/src/audio/input_devices.rs',
        'rust/ai-tutor/src/voice_loop.rs', 'rust/ai-tutor/src/segment_speech.rs',
        'rust/ai-tutor/src/context.rs', 'rust/ai-tutor/src/server.rs',
        'scripts/validation/voice_model_chain_smoke.py',
        'scripts/speech-lab/probe_managed_tts.py', 'prompts/tutor_core.md',
        'implementation-package/AI_Tutor_Codex_Implementation_Package/schemas/tutor_reply.schema.json',
    ]
    method = {
        'method': 'paced-synthetic-input-real-voice-loop-v1-20261104', 'test': TEST,
        'test_executable_sha256': digest(executable), 'runner_sha256': digest(Path(__file__)),
        'source_sha256': {name: digest(root / name) for name in source_names},
        'languages': ['zh', 'en'], 'rounds_per_language': 2,
        'automatic_retry': False, 'teaching_requests_per_language': 2,
        'bundle_sha256': hashes, 'model_revision': MODEL_REVISION,
        'model_manifest_sha256': digest(model / 'download-manifest.json'),
        'model_files_verified': len(manifest['files']), 'model_files_hard_linked': len(links),
        'environment_pointer_sha256': digest(fixture / relative),
        'previous_fixture_report_sha256': digest(previous_root / 'report.json'),
        'warmup': [{'language': language, 'text': text, 'seed': seed} for language, text, seed in WARM],
        'readiness_budget_seconds': 240, 'warmup_budget_per_language_seconds': 120,
        'diagnostic_budget_per_language_seconds': ROUND_BUDGET_SECONDS,
        'token_transport': 'Private wrapper stdin; fresh credential in child environment, redacted in logs',
        'microphone_opened': False, 'native_output_requested': True,
        'speaker_output_measured': False, 'simulated_display_ack': True,
        'production_descriptor_or_policy_written': False, 'raw_pcm_saved': False,
        'scope': 'Same Workstation production capture/VAD/ASR, voice-loop transitions, teaching, native playback completion and rearm. Fixed paced WAV input and simulated display ACK. No microphone, browser, acoustic or 30-minute acceptance.',
    }
    save(output / 'method.json', method)
    return fixture


def check_voice_loop(value, language):
    if (value.get('test_type') != 'real_synthetic_voice_loop_two_rounds'
            or value.get('language') != language or value.get('passed') is not True):
        raise ProbeFailure('voice_loop_report_identity_failed')
    for key in ('production_capture_loop', 'synthetic_capture_source', 'production_audio_owner',
                'simulated_display_ack', 'protected_files_unchanged', 'provider_cleanup_confirmed'):
        if value.get(key) is not True:
            raise ProbeFailure('voice_loop_required_proof_missing_' + key)
    for key in ('microphone_opened', 'camera_opened', 'ptz_enabled', 'raw_pcm_saved', 'speaker_output_measured'):
        if value.get(key) is not False:
            raise ProbeFailure('voice_loop_scope_mismatch_' + key)
    owned = value.get('owned_asr_stop', {})
    if any(owned.get(key) is not True for key in (
            'cleanup_confirmed', 'child_exited', 'interpreter_exited',
            'launcher_job_empty', 'interpreter_job_empty', 'interpreter_job_owned')):
        raise ProbeFailure('voice_loop_asr_cleanup_unconfirmed')
    metrics = value.get('provider_metrics', {})
    if metrics.get('requests') != 2 or metrics.get('acp_segment_requests') != 2:
        raise ProbeFailure('voice_loop_model_call_count_changed')
    lifecycle = value.get('source_lifecycle', {})
    if lifecycle.get('opens') != 2 or lifecycle.get('closes') != 2:
        raise ProbeFailure('voice_loop_source_not_closed_exactly_twice')
    rounds = value.get('rounds', [])
    if len(rounds) != 2 or [row.get('case') for row in rounds] != ['round_1', 'round_2']:
        raise ProbeFailure('voice_loop_round_count_changed')
    if any(row.get('completed') is not True or row.get('rearm_observed') is not True for row in rounds):
        raise ProbeFailure('voice_loop_playback_or_rearm_incomplete')


def run_language(root, output, executable, language, token, process, events, report, started):
    run = 'voice-loop-model-' + uuid.uuid4().hex + '-' + language
    env = os.environ.copy()
    env.update(AI_TUTOR_REAL_VOICE_LOOP='1', AI_TUTOR_REAL_VOICE_CHAIN_SYNTHETIC='1',
               AI_TUTOR_VOICE_CHAIN_LANGUAGE=language, AI_TUTOR_VOICE_CHAIN_TTS_ENGINE='qwen',
               AI_TUTOR_VOICE_CHAIN_RUN=run, AI_TUTOR_VOICE_CHAIN_TTS_TOKEN=token)
    child = subprocess.Popen([str(executable), TEST, '--exact', '--ignored', '--nocapture', '--test-threads=1'],
                             cwd=root, env=env, stdin=subprocess.DEVNULL,
                             stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                             creationflags=subprocess.CREATE_NO_WINDOW)
    env.pop('AI_TUTOR_VOICE_CHAIN_TTS_TOKEN')
    interrupted = False
    try:
        stdout, stderr = child.communicate(timeout=ROUND_BUDGET_SECONDS)
    except (subprocess.TimeoutExpired, KeyboardInterrupt):
        interrupted = True
        child.terminate()
        try:
            stdout, stderr = child.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            child.kill()
            stdout, stderr = child.communicate(timeout=5)
    for name, data in ((language + '.stdout.log', stdout), (language + '.stderr.log', stderr)):
        (output / name).write_bytes(data.replace(token.encode('ascii'), b'[REDACTED]'))
    path = root / 'runtime/goal-baselines/20261104' / run / 'report.json'
    result = {'language': language, 'exit_code': child.returncode, 'interrupted': interrupted,
              'report': str(path), 'success': False}
    report['chains'].append(result)
    if interrupted or child.returncode != 0:
        raise ProbeFailure('voice_loop_process_failed_no_retry')
    check_voice_loop(json.loads(path.read_text(encoding='utf-8')), language)
    require_running(process, events, report, started)
    result.update(success=True, report_sha256=digest(path))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--real-local-synthetic', action='store_true', required=True)
    parser.add_argument('--root', type=Path, required=True)
    parser.add_argument('--test-executable', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    root, output, executable = args.root.resolve(), args.output.resolve(), args.test_executable.resolve()
    if os.name != 'nt' or not executable.is_file():
        parser.error('Requires Windows and an explicitly selected prebuilt library test executable')
    output.mkdir(parents=True, exist_ok=False)
    instance, token = uuid.uuid4().hex, secrets.token_urlsafe(32)
    report = {'test_type': 'owned_qwen_real_synthetic_voice_loop', 'success': False,
              'instance_id': instance, 'starting': False, 'ready': False, 'warm': False,
              'stopped': False, 'linux_cleanup_confirmed': False, 'bridge_terminated_only': False,
              'wrapper_events': [], 'authentication': {}, 'errors': [], 'warmup': [], 'chains': [],
              'microphone_opened': False, 'native_output_requested': True,
              'speaker_output_measured': False, 'simulated_display_ack': True, 'raw_pcm_saved': False,
              'automatic_retry': False, 'production_descriptor_or_policy_written': False}
    process, readers, events, before = None, [], queue.Queue(), None
    started = time.monotonic()
    try:
        before = protected(root)
        report['protected_before_sha256'] = before
        fixture = stage(root, output, executable)
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
        for language in ('zh', 'en'):
            require_running(process, events, report, started)
            run_language(root, output, executable, language, token, process, events, report, started)
            save(output / 'report.json', report)
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
                             and report['protected_files_unchanged'] and len(report['chains']) == 2
                             and all(row['success'] for row in report['chains']))
        save(output / 'report.json', report)
    print(json.dumps({'success': report['success'], 'report': str(output / 'report.json'),
                      'chains_completed': sum(row['success'] for row in report['chains']),
                      'linux_cleanup_confirmed': report['linux_cleanup_confirmed']}))
    return 0 if report['success'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
