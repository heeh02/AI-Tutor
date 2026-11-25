"""Two fixed voice rounds and same-Workstation managed services/shadow memory.

The runner stages assets and observes one already-built ignored test process.
It never starts, contacts or stops a model service itself. No automatic retry.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import time

from voice_loop_soak_resources import ResourceSampler
from voice_loop_soak_model_smoke import protected, selected_camera
from voice_model_chain_smoke import MODEL_REVISION, ProbeFailure, digest

TEST = 'server::voice_memory_managed_diagnostic::real_managed_voice_camera_memory'
METHOD = 'managed-workstation-camera-qwen-final-shadow-v1-20261106'
SOAK_METHOD = 'managed-workstation-camera-qwen-soak-v1-20261106'
SOAK_TEST = 'server::voice_memory_managed_diagnostic::real_managed_voice_camera_memory_soak'
FLASH_METHOD = 'managed-workstation-camera-qwen-flash-joint-v1-20261109'
FLASH_TEST = 'server::voice_memory_managed_diagnostic::real_managed_voice_camera_memory_flash_joint'
FLASH_ARTIFACTS = ('prompts/tutor_core.md', 'prompts/tutor-agent.md',
    'implementation-package/AI_Tutor_Codex_Implementation_Package/schemas/tutor_reply.schema.json',
    'runtime/kimi/verification-rust.json')
GRAPH_SHA = '3efa38e607d4d53d7eb7e44556c03373b6f069f1407c0957f24f6fc668820f3e'
BUNDLE = ('managed_tts.py', 'tts_gpu_lease.sh', 'gpu_admission.py', 'cuda_link_env.sh',
          'start_qwen_omni.sh', 'start_cosy_omni.sh', 'qwen3_tts_12gb.yaml',
          'cosyvoice3_12gb_candidate.yaml')
FINAL_BUNDLE = ('managed_final_asr.py', 'qwen_final_engine.py', 'qwen_final_service.py',
                'qwen-model.json', 'qwen-sdk.json')
FINAL_PROVIDER = 'Qwen3-ASR-1.7B / local GPU final'
FINAL_REVISION = '7278e1e70fe206f11671096ffdd38061171dd6e5'
FINAL_MODEL = 'models/speech/Qwen3-ASR-1.7B-7278e1e7'
CPU_BUNDLE = ('managed_asr.py', 'asr_service.py', 'recognizer.py')
SOURCES = (
    'rust/ai-tutor/src/server.rs', 'rust/ai-tutor/src/voice_memory_managed_diagnostic.rs',
    'rust/ai-tutor/src/voice_memory_managed_tests.rs',
    'rust/ai-tutor/src/voice_memory_managed_soak.rs',
    'rust/ai-tutor/src/context.rs', 'rust/ai-tutor/src/audio/input_devices.rs',
    'rust/ai-tutor/src/voice_loop_model_diagnostic.rs', 'rust/ai-tutor/src/voice_loop_soak_diagnostic.rs',
    'rust/ai-tutor/src/voice_loop.rs', 'rust/ai-tutor/src/recording.rs',
    'rust/ai-tutor/src/audio.rs', 'rust/ai-tutor/src/audio/voice_loop_source.rs',
    'rust/ai-tutor/src/audio/voice_model_chain_diagnostic.rs',
    'rust/ai-tutor/src/audio/voice_endpoint.rs', 'rust/ai-tutor/src/audio/capture_endpoint.rs',
    'rust/ai-tutor/src/local_speech_runtime.rs', 'rust/ai-tutor/src/local_speech.rs',
    'rust/ai-tutor/src/local_speech/managed.rs', 'rust/ai-tutor/src/local_asr_runtime.rs',
    'rust/ai-tutor/src/local_speech/managed_prepare.rs',
    'rust/ai-tutor/src/local_asr.rs', 'rust/ai-tutor/src/local_asr/owned.rs',
    'rust/ai-tutor/src/gpu_resource_policy.rs',
    'rust/ai-tutor/src/gpu_queue.rs',
    'rust/ai-tutor/src/final_asr.rs', 'rust/ai-tutor/src/final_asr/owned.rs',
    'rust/ai-tutor/src/final_asr_runtime.rs', 'rust/ai-tutor/src/audio/final_transcript.rs',
    'rust/ai-tutor/src/audio/streaming_asr.rs',
    'rust/ai-tutor/src/memory_schedule.rs', 'rust/ai-tutor/src/memory_worker.rs',
    'rust/ai-tutor/src/memory_contract.rs', 'rust/ai-tutor/src/store.rs',
    'rust/ai-tutor/src/store/consolidation.rs', 'rust/ai-tutor/src/store/memory_policy.rs',
    'rust/ai-tutor/src/store/memory_discovery.rs', 'rust/ai-tutor/src/store/education.rs',
    'rust/ai-tutor/src/kimi.rs', 'rust/ai-tutor/src/kimi_acp_stream.rs',
    'rust/ai-tutor/src/kimi_acp_common.rs', 'rust/ai-tutor/src/tutor.rs',
    'rust/ai-tutor/src/foreground_provider.rs', 'rust/ai-tutor/src/deepseek_client.rs',
    'rust/ai-tutor/src/deepseek_flash_sse.rs', 'rust/ai-tutor/src/kimi_acp_memory.rs',
    'rust/ai-tutor/src/kimi_acp_probe.rs',
    'rust/ai-tutor/src/tutor/citation.rs', 'rust/ai-tutor/src/pending_step.rs',
    'rust/ai-tutor/src/tutor_segments.rs',
    'rust/ai-tutor/src/tutor_delivery.rs', 'rust/ai-tutor/src/segment_speech.rs',
    'rust/ai-tutor/src/store/tutor_delivery.rs', 'rust/ai-tutor/src/store/tutor_speech.rs',
    'rust/ai-tutor/src/vision.rs', 'rust/ai-tutor/src/vision_test_worker.rs',
    'scripts/validation/managed_voice_memory_cases_v1.json',
    'scripts/validation/managed_voice_memory_method_qwen_v1.json',
    'scripts/validation/managed_voice_memory_soak_v1.json',
    'scripts/validation/managed_voice_flash_joint_v1.json',
    'scripts/validation/voice_memory_managed_smoke.py',
    'scripts/validation/voice_loop_soak_resources.py',
    'scripts/validation/voice_loop_soak_model_smoke.py',
    'scripts/validation/voice_model_chain_smoke.py',
    'prompts/tutor_core.md', 'prompts/tutor-agent.md', 'prompts/memory_consolidation.md',
    'implementation-package/AI_Tutor_Codex_Implementation_Package/schemas/tutor_reply.schema.json',
)


def write_new(path, value):
    with path.open('x', encoding='utf-8') as stream:
        json.dump(value, stream, ensure_ascii=False, indent=2)
        stream.write('\n')


def protections(root):
    result = protected(root)
    for name in ('runtime/memory-policy.json', 'prompts/memory_consolidation.md',
                 'runtime/gpu-resource-policy.json', 'runtime/local-final-asr-policy.json',
                 'runtime/teaching-provider-policy.json', 'runtime/kimi/verification-rust.json',
                 'runtime/goal-baselines/20261103/sapi-corpus/zh_short.wav',
                 'runtime/goal-baselines/20261103/sapi-corpus/en_short.wav'):
        result[name] = digest(root / name) if (root / name).is_file() else None
    return result


def verified_build_binding(root, build_manifest, executable, worker):
    measured = json.loads(build_manifest.read_text(encoding='utf-8'))
    executables = {row['name']: row['sha256'] for row in measured['executables']}
    if any(executables.get(path.name) != digest(path) for path in (executable, worker)):
        raise ProbeFailure('measured_executable_mismatch')
    # The runner is interpreted independently; bind compiled Rust, embedded
    # fixtures, policy/schema and the actual staged service bundle to the build.
    # Python runner revisions are recorded separately in source_sha256 below.
    required = {name for name in SOURCES if not name.endswith('.py')}
    required.update('scripts/speech-lab/' + name for name in BUNDLE)
    required.update('scripts/final-asr/' + name for name in FINAL_BUNDLE)
    required.update('scripts/speech-service/' + name for name in CPU_BUNDLE)
    sources = measured['source_files_sha256']
    if any(sources.get(name) != digest(root / name) for name in required):
        raise ProbeFailure('measured_runtime_source_mismatch')
    return {'manifest_sha256': digest(build_manifest),
            'verified_source_count': len(required),
            'bundle_sha256': {name: sources['scripts/speech-lab/' + name] for name in BUNDLE},
            'final_bundle_sha256': {name: sources['scripts/final-asr/' + name] for name in FINAL_BUNDLE},
            'cpu_bundle_sha256': {name: sources['scripts/speech-service/' + name] for name in CPU_BUNDLE}}


def stage_final_asr(root, fixture, binding):
    bundle = fixture / 'bin/final-asr'
    bundle.mkdir(parents=True)
    hashes = {}
    for name in FINAL_BUNDLE + ('gpu_admission.py',):
        source = root / ('scripts/speech-lab' if name == 'gpu_admission.py' else 'scripts/final-asr') / name
        expected = (binding['bundle_sha256'] if name == 'gpu_admission.py'
                    else binding['final_bundle_sha256'])[name]
        if digest(source) != expected:
            raise ProbeFailure('final_bundle_differs_from_measured_build')
        shutil.copyfile(source, bundle / name)
        hashes[name] = digest(bundle / name)
        if hashes[name] != expected:
            raise ProbeFailure('final_bundle_copy_changed')
    manifest = json.loads((bundle / 'qwen-model.json').read_text(encoding='utf-8'))
    if (manifest.get('model_id') != 'Qwen/Qwen3-ASR-1.7B'
            or manifest.get('model_revision') != FINAL_REVISION):
        raise ProbeFailure('fixed_final_model_changed')
    model = fixture / FINAL_MODEL
    model.mkdir(parents=True)
    verified = []
    for item in manifest['files']:
        if item['runtime_required'] is not True:
            continue
        name = item['path']
        if Path(name).name != name or '/' in name or '\\' in name:
            raise ProbeFailure('final_asset_name_invalid')
        source = root / FINAL_MODEL / name
        if source.stat().st_size != item['size_bytes']:
            raise ProbeFailure('final_model_size_changed')
        check = hashlib.sha256() if item['lfs_sha256'] else hashlib.sha1()
        if not item['lfs_sha256']:
            check.update(f"blob {item['size_bytes']}\0".encode('ascii'))
        with source.open('rb') as stream:
            for block in iter(lambda: stream.read(4 * 1024 * 1024), b''):
                check.update(block)
        if check.hexdigest() != (item['lfs_sha256'] or item['git_blob_sha1']):
            raise ProbeFailure('final_model_hash_changed')
        os.link(source, model / name)
        if not source.samefile(model / name):
            raise ProbeFailure('final_model_hardlink_changed')
        verified.append(name)
    if len(verified) != 10:
        raise ProbeFailure('fixed_final_runtime_asset_count_changed')
    return {'bundle_sha256': hashes, 'model_revision': FINAL_REVISION,
            'runtime_assets_verified': verified, 'model_manifest_sha256': hashes['qwen-model.json']}


def select_method(soak=False, flash_joint=False):
    if soak and flash_joint:
        raise ProbeFailure('flash_joint_and_soak_are_mutually_exclusive')
    if flash_joint:
        return FLASH_METHOD, FLASH_TEST, 'managed_voice_flash_joint_v1.json'
    if soak:
        return SOAK_METHOD, SOAK_TEST, 'managed_voice_memory_soak_v1.json'
    return METHOD, TEST, 'managed_voice_memory_method_qwen_v1.json'


def stage_flash_provider(root, fixture):
    copied = {}
    for name in FLASH_ARTIFACTS:
        source, target = root / name, fixture / name
        expected = digest(source)
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, target)
        if digest(target) != expected or digest(source) != expected:
            raise ProbeFailure('foreground_fixture_artifact_changed')
        copied[name] = expected
    policy = {'protocol_version': 1, 'provider': 'deepseek_flash', 'thinking': 'non_thinking'}
    write_new(fixture / 'runtime/teaching-provider-policy.json', policy)
    return {'policy': policy, 'copied_sha256': copied,
            'credential_copied': False, 'remote_verification_performed': False}


def stage(root, output, executable, worker, camera_sha, distribution, build_manifest,
          final_python, final_cache, soak=False, flash_joint=False):
    method_name, test_name, method_file = select_method(soak, flash_joint)
    binding = verified_build_binding(root, build_manifest, executable, worker)
    previous_dir = root / 'runtime/goal-baselines/20261104/main-managed-qwen-first'
    previous = json.loads((previous_dir / 'report.json').read_text(encoding='utf-8'))
    if previous.get('success') is not True or previous.get('main_exit_code') != 0:
        raise ProbeFailure('prior_managed_asset_provenance_missing')
    fixture = output / 'fixture'
    bundle = fixture / 'bin/local-speech'
    bundle.mkdir(parents=True)
    hashes = {}
    for name in BUNDLE:
        source = root / 'scripts/speech-lab' / name
        if digest(source) != binding['bundle_sha256'][name]:
            raise ProbeFailure('managed_bundle_differs_from_measured_build')
        shutil.copyfile(source, bundle / name)
        hashes[name] = digest(bundle / name)
        if hashes[name] != digest(source):
            raise ProbeFailure('managed_bundle_copy_changed')
    if hashes['qwen3_tts_12gb.yaml'] != GRAPH_SHA:
        raise ProbeFailure('qwen_graph_changed')
    links = previous['model_files_hard_linked']
    for name in links:
        source = root / 'models' / name
        old = previous_dir / 'fixture/models' / name
        if not source.samefile(old):
            raise ProbeFailure('verified_model_hardlink_changed')
        target = fixture / 'models' / name
        target.parent.mkdir(parents=True, exist_ok=True)
        os.link(source, target)
    model = fixture / 'models/speech/Qwen3-TTS-12Hz-0.6B-CustomVoice'
    manifest = json.loads((model / 'download-manifest.json').read_text(encoding='utf-8'))
    if manifest['revision'] != MODEL_REVISION:
        raise ProbeFailure('qwen_revision_changed')
    for row in manifest['files']:
        if digest(model / row['file']) != row['sha256']:
            raise ProbeFailure('qwen_model_hash_changed')
    final_assets = stage_final_asr(root, fixture, binding)
    pointer = fixture / 'runtime/speech-gpu/omni-env-path.txt'
    pointer.parent.mkdir(parents=True)
    # bash consumes this pointer directly; Windows CRLF would become part of PATH.
    pointer.write_bytes(((root / 'runtime/speech-gpu/omni-env-path.txt')
                         .read_text(encoding='utf-8').strip() + '\n').encode('utf-8'))
    speech = {'protocol_version': 1, 'mode': 'managed', 'warmup': 'synthetic_bilingual',
              'managed': {'engine': 'qwen', 'distribution': distribution}}
    asr_python = (root / 'runtime/speech-lab/.venv/Scripts/python.exe').resolve(strict=True)
    asr = {'protocol_version': 1, 'mode': 'managed', 'python_executable': str(asr_python)}
    if not all(value.startswith('/') and '\0' not in value for value in (final_python, final_cache)):
        raise ProbeFailure('absolute_linux_final_paths_required')
    final_asr = {'protocol_version': 1, 'enabled': True,
                 'managed': {'distribution': distribution, 'python_executable': final_python,
                             'cache_directory': final_cache}}
    gpu = {'protocol_version': 1, 'mode': 'qwen_pair_12gb_v1'}
    write_new(fixture / 'runtime/local-speech-policy.json', speech)
    write_new(fixture / 'runtime/local-asr-policy.json', asr)
    write_new(fixture / 'runtime/local-final-asr-policy.json', final_asr)
    write_new(fixture / 'runtime/gpu-resource-policy.json', gpu)
    foreground_staging = stage_flash_provider(root, fixture) if flash_joint else None
    method_path = root / 'scripts/validation' / method_file
    cases_path = root / 'scripts/validation/managed_voice_memory_cases_v1.json'
    method = json.loads(method_path.read_text(encoding='utf-8'))
    if method['method'] != method_name or method['test'] != test_name:
        raise ProbeFailure('fixed_method_identity_changed')
    setup = {'method': method_name, 'test': test_name, 'test_executable_sha256': digest(executable),
             'camera_worker': str(worker), 'camera_worker_sha256': digest(worker),
             'camera_id_sha256': camera_sha, 'model_service_owned_by_runner': False,
             'bundle_sha256': hashes, 'model_revision': MODEL_REVISION,
             'final_asr_assets': final_assets,
             'model_manifest_sha256': digest(model / 'download-manifest.json'),
             'model_files_hard_linked': len(links), 'model_files_verified': len(manifest['files']),
             'environment_pointer_sha256': digest(pointer),
             'policies': {'speech': speech, 'asr': asr, 'final_asr': final_asr, 'gpu': gpu},
             'cases_sha256': digest(cases_path), 'method_sha256': digest(method_path),
             'source_sha256': {name: digest(root / name) for name in SOURCES},
             'measured_build_binding': binding,
             'source_hash_scope': 'Compiled Rust/fixtures, policy/schema and service bundle match the explicit build manifest. Interpreted Python runner sources are separately recorded at invocation.',
             'raw_tokens_saved': False, 'production_policy_written': False}
    if foreground_staging is not None:
        setup['foreground_staging'] = foreground_staging
    write_new(output / 'setup.json', setup)
    return method


def check_report(value):
    if value.get('test_type') != 'managed_workstation_camera_voice_shadow' or value.get('passed') is not True:
        raise ProbeFailure('joint_report_not_passed')
    soak = value.get('method', {}).get('method') == SOAK_METHOD
    flash_joint = value.get('method', {}).get('method') == FLASH_METHOD
    if (value.get('method', {}).get('method') not in (METHOD, SOAK_METHOD, FLASH_METHOD)
            or value.get('expected_asr_provider') != FINAL_PROVIDER):
        raise ProbeFailure('qwen_joint_method_or_provider_changed')
    for name in ('operation_completed', 'cleanup_confirmed', 'scheduler_join_confirmed',
                 'provider_cleanup_confirmed', 'protected_files_unchanged',
                 'heartbeat_finished_cleanly', 'camera_sampler_finished_cleanly',
                 'final_asr_cleanup_confirmed', 'gpu_pair_observed_active_at_completion'):
        if value.get(name) is not True:
            raise ProbeFailure('joint_required_proof_missing_' + name)
    for name in ('microphone_opened', 'ptz_enabled', 'speaker_output_measured',
                 'remote_generation_inflight_preemption_proven',
                 'raw_media_saved', 'release_main_startup_tested',
                 'browser_tested', 'acoustic_loop_tested'):
        if value.get(name) is not False:
            raise ProbeFailure('joint_scope_mismatch_' + name)
    if value.get('thirty_minute_soak') is not soak:
        raise ProbeFailure('soak_scope_flag_mismatch')
    rounds = value.get('rounds', [])
    count_ok = 2 <= len(rounds) <= 96 if soak else len(rounds) == 2
    if not count_ok or any(row.get('completed') is not True or row.get('passed') is not True
                               or row.get('submitted_actual_asr_text_unchanged') is not True
                               or row.get('expected_asr_provider') != FINAL_PROVIDER
                               or row.get('asr', {}).get('provider') != FINAL_PROVIDER
                               or row.get('asr', {}).get('fallback_reason') is not None
                               for row in rounds):
        raise ProbeFailure('two_foreground_rounds_not_complete')
    final_stop = value.get('final_asr_after_cleanup', {})
    owner_stop = final_stop.get('ownership_stop', {})
    terminal = owner_stop.get('receipt', {})
    if (final_stop.get('cleanup_confirmed') is not True or final_stop.get('active') is not False
            or final_stop.get('busy') is not False
            or owner_stop.get('cleanup_confirmed') is not True
            or owner_stop.get('protocol_failed') is not False
            or owner_stop.get('linux_cleanup_confirmed') is not True
            or owner_stop.get('normal_owner_stop') is not True
            or owner_stop.get('bridge_exit_code') != 0
            or terminal.get('state') != 'owned_stopped'
            or terminal.get('linux_cleanup_confirmed') is not True
            or terminal.get('process_started') is not True
            or terminal.get('service_exit_code') != 0 or terminal.get('forced_group_stop') is not False):
        raise ProbeFailure('final_asr_owned_cleanup_incomplete')
    for name in ('A', 'B'):
        job = value.get('jobs', {}).get(name, {})
        preempt = job.get('preemption', {})
        recovered = job.get('recovery', {})
        terminal = recovered.get('job', {})
        if (preempt.get('covered') is not True or preempt.get('observed_pending_without_candidates') is not True
                or recovered.get('same_job_recovered') is not True
                or recovered.get('substantive_type_source_coverage') is not True
                or terminal.get('attempts') != 2 or terminal.get('second_lease_differs') is not True
                or not terminal.get('candidates') or terminal.get('status') != 'shadow_ready'):
            raise ProbeFailure('background_preemption_or_substantive_recovery_missing_' + name)
    if value.get('memory_counts') != {'jobs': 2, 'reservations': 4, 'starts': 4, 'background_memories': 0}:
        raise ProbeFailure('background_persistent_budget_changed')
    audit = value.get('final_memory_audit', {})
    check_memory_audit(audit)
    if soak:
        check_soak(value, rounds)
    elif flash_joint:
        check_flash_joint(value, rounds)
    if value.get('camera', {}).get('observation_coverage_complete') is not True:
        raise ProbeFailure('camera_coverage_incomplete')
    if value.get('independent_semantic_review') != 'pending':
        raise ProbeFailure('semantic_review_scope_changed')


def check_memory_audit(audit):
    records = audit.get('records')
    if (audit.get('namespace') != 'simulator' or audit.get('all_statuses_included') is not True
            or not isinstance(records, list) or len(records) != audit.get('whole_database_memory_count')
            or sum(row.get('learner_id') == audit.get('voice_learner_id') for row in records)
               != audit.get('voice_learner_memory_count')
            or any(not all(key in row for key in ('content', 'provenance_links', 'current_sources',
                                                   'missing_or_inactive_source_ids')) for row in records)):
        raise ProbeFailure('final_memory_content_or_source_evidence_missing')


def check_flash_joint(value, rounds):
    if (value.get('foreground_provider') != 'deepseek_flash'
            or value.get('language') != 'zh_then_en'
            or value.get('automatic_teaching_retry') is not False
            or [row.get('language') for row in rounds] != ['zh', 'en']):
        raise ProbeFailure('flash_bilingual_scope_changed')
    preflight = value.get('provider_preflight', {})
    foreground, kimi = preflight.get('foreground', {}), preflight.get('kimi', {})
    if (foreground.get('provider') != 'deepseek_flash' or foreground.get('available') is not True
            or foreground.get('thinking') != 'non_thinking'
            or foreground.get('stream_contract') != 'metadata_v2'
            or foreground.get('reply_text_origin') != 'server_segment_concatenation'
            or foreground.get('readiness_basis') != 'local_client_and_credential_preflight_only'
            or foreground.get('remote_verified_by_status_poll') is not False
            or kimi.get('available') is not True or kimi.get('verified') is not True
            or kimi.get('tools') != []
            or len(preflight.get('verification_receipt_sha256', '')) != 64):
        raise ProbeFailure('flash_or_kimi_preflight_missing')
    transition = value.get('language_transition', {})
    if (transition.get('completed') is not True or transition.get('from') != 'zh'
            or transition.get('to') != 'en' or not 0 <= transition.get('elapsed_ms', -1) <= 5000
            or transition.get('normal_settings_and_enable_apis') is not True
            or transition.get('camera_reopened') is not False):
        raise ProbeFailure('bounded_bilingual_transition_missing')
    expected_hashes = (
        'ad72efd95a1abc540737554f9ccb63b7a9933effb75e5e458824a90363bb9a33',
        '670191397605e3f21f2b0e331852b665f91e5ee21cb9573f2ffdd93aff825a9f',
    )
    request_ids = set()
    for index, row in enumerate(rounds, 1):
        if (row.get('case') != 'round_' + str(index)
                or row.get('source_id') != row['language'] + '_short'
                or row.get('source_sha256') != expected_hashes[index - 1]
                or row.get('source_round_index') != 1 or row.get('user_turn_total') != index
                or row.get('source_lifecycle', {}).get('opens') != 1
                or row.get('source_lifecycle', {}).get('closes') != 1
                or row.get('protected_files_unchanged') is not True
                or row.get('gpu_pair_active_same_tts_instance') is not True
                or not value.get('final_asr_instance_id')
                or row.get('same_final_asr_instance') is not True
                or row.get('final_asr_instance_id') != value['final_asr_instance_id']
                or row.get('whisper_fallback_used') is not False
                or row.get('shadow_preemption') != ('A' if index == 1 else 'B')):
            raise ProbeFailure('flash_round_source_or_ownership_changed')
        check_memory_audit(row.get('memory_audit', {}))
        submit = row.get('submit', {})
        diagnostic = row.get('provider_diagnostics', {})
        metrics = diagnostic.get('last_foreground_segments', {})
        if (not submit.get('request_id') or not isinstance(submit.get('generation'), int)
                or submit['generation'] <= 0 or submit['request_id'] in request_ids
                or diagnostic.get('matched') is not True
                or diagnostic.get('request_id') != submit['request_id']
                or diagnostic.get('generation') != submit['generation']
                or not isinstance(metrics, dict)
                or metrics.get('request_id') != submit['request_id']
                or metrics.get('generation') != submit['generation']
                or metrics.get('transport') != 'deepseek_http'
                or metrics.get('stream_contract') != 'metadata_v2'
                or metrics.get('reply_text_origin') != 'server_segment_concatenation'
                or metrics.get('operation_success') is not True
                or metrics.get('success') is not True):
            raise ProbeFailure('flash_round_provider_identity_or_contract_changed')
        request_ids.add(submit['request_id'])
    for key in ('provider_before_cleanup', 'provider_after_cleanup'):
        metrics = value.get(key, {})
        if (metrics.get('requests') != 6 or metrics.get('deepseek_segment_requests') != 2
                or metrics.get('acp_segment_requests', 0) != 0
                or metrics.get('consolidation_requests') != 4 or metrics.get('retries') != 0):
            raise ProbeFailure('flash_provider_budget_changed')
    sources = value.get('source_lifecycles', [])
    if (len(sources) != 2
            or any(source.get('active') is not False or source.get('opens') != 1
                   or source.get('closes') != 1 or len(source.get('closed_streams', [])) != 1
                   or source['closed_streams'][0].get('producer_joined') is not True
                   for source in sources)):
        raise ProbeFailure('flash_capture_ownership_incomplete')


def check_soak(value, rounds):
    soak = value.get('soak', {})
    if (not 1800000 <= value.get('active_duration_ms', 0) <= 2100000
            or soak.get('active_window_reached') is not True
            or soak.get('minimum_active_ms') != 1800000
            or soak.get('foreground_limit') != 96 or soak.get('idle_padding') is not False
            or soak.get('late_b_recovered') is not True
            or soak.get('bilingual_phases_completed') is not True):
        raise ProbeFailure('soak_duration_or_budget_incomplete')
    if not 0 <= value.get('jobs', {}).get('A', {}).get('started_active_ms', -1) < 120000:
        raise ProbeFailure('early_background_not_observed')
    if not 1500000 <= value.get('jobs', {}).get('B', {}).get('started_active_ms', -1) < 1800000:
        raise ProbeFailure('late_background_not_observed')
    transition = value.get('language_transition', {})
    if (transition.get('completed') is not True or transition.get('from') != 'zh'
            or transition.get('to') != 'en' or not 0 <= transition.get('elapsed_ms', -1) <= 5000
            or transition.get('at_active_ms', 0) < 900000
            or transition.get('normal_settings_and_enable_apis') is not True
            or transition.get('camera_reopened') is not False):
        raise ProbeFailure('bounded_bilingual_transition_missing')
    expected = {
        'zh': ('zh_short', 'ad72efd95a1abc540737554f9ccb63b7a9933effb75e5e458824a90363bb9a33'),
        'en': ('en_short', '670191397605e3f21f2b0e331852b665f91e5ee21cb9573f2ffdd93aff825a9f'),
    }
    counts = {'zh': 0, 'en': 0}
    seen_en = False
    previous = None
    preemptions = []
    for index, row in enumerate(rounds, 1):
        language = row.get('language')
        if language not in expected or (seen_en and language == 'zh'):
            raise ProbeFailure('soak_language_sequence_changed')
        seen_en = seen_en or language == 'en'
        counts[language] += 1
        if (row.get('case') != 'round_' + str(index)
                or (row.get('source_id'), row.get('source_sha256')) != expected[language]
                or row.get('source_round_index') != counts[language]
                or row.get('source_lifecycle', {}).get('opens') != counts[language]
                or row.get('source_lifecycle', {}).get('closes') != counts[language]
                or row.get('user_turn_total') != index
                or row.get('protected_files_unchanged') is not True
                or row.get('gpu_pair_active_same_tts_instance') is not True
                or row.get('whisper_fallback_used') is not False
                or not 0 <= row.get('soak_started_ms', -1) < 1800000
                or row.get('soak_finished_ms', -1) < row['soak_started_ms']):
            raise ProbeFailure('soak_round_evidence_incomplete')
        check_memory_audit(row.get('memory_audit', {}))
        if previous is not None:
            gap = row['soak_started_ms'] - previous['soak_finished_ms']
            allowed = 120000 if previous.get('shadow_preemption') in ('A', 'B') else 10000
            if not 0 <= gap <= allowed:
                raise ProbeFailure('soak_unexplained_workload_gap')
        if row.get('shadow_preemption') is not None:
            preemptions.append(row['shadow_preemption'])
        previous = row
    if preemptions != ['A', 'B'] or rounds[0]['soak_started_ms'] >= 120000:
        raise ProbeFailure('soak_preemption_sequence_changed')
    for begin in range(0, 1800000, 300000):
        if not any(row['soak_started_ms'] < begin + 300000 and row['soak_finished_ms'] > begin
                   for row in rounds):
            raise ProbeFailure('soak_five_minute_window_has_no_foreground')
    sources = value.get('source_lifecycles', [])
    if (not all(counts.values()) or len(sources) != 2
            or sum(row.get('opens', 0) for row in sources) != len(rounds)
            or any(row.get('active') is not False or row.get('opens') != row.get('closes')
                   or len(row.get('closed_streams', [])) != row.get('opens')
                   or any(item.get('producer_joined') is not True
                          for item in row.get('closed_streams', [])) for row in sources)):
        raise ProbeFailure('soak_capture_ownership_incomplete')


def run_child(root, output, executable, camera_id, method, report):
    env = os.environ.copy()
    env.pop('AI_TUTOR_VOICE_CHAIN_TTS_TOKEN', None)
    env.pop('AI_TUTOR_REAL_MANAGED_VOICE_MEMORY_SOAK', None)
    env.pop('AI_TUTOR_REAL_FLASH_VOICE_JOINT', None)
    if method['method'] == SOAK_METHOD:
        env['AI_TUTOR_REAL_MANAGED_VOICE_MEMORY_SOAK'] = '1'
    if method['method'] == FLASH_METHOD:
        env['AI_TUTOR_REAL_FLASH_VOICE_JOINT'] = '1'
    env.update(AI_TUTOR_REAL_MANAGED_VOICE_MEMORY='1',
               AI_TUTOR_REAL_VOICE_CHAIN_SYNTHETIC='1',
               AI_TUTOR_MANAGED_JOINT_OUTPUT=str(output), AI_TUTOR_SOAK_CAMERA_ID=camera_id)
    started = time.monotonic()
    sampler = child = None
    interrupted = False
    # Only bounded structured diagnostics are retained. Never persist arbitrary
    # child/provider stdout/stderr, which can contain remote text or credentials.
    with open(os.devnull, 'wb') as stdout, open(os.devnull, 'wb') as stderr:
        child = subprocess.Popen([str(executable), method['test'], '--exact', '--ignored', '--nocapture', '--test-threads=1'],
                                 cwd=root, env=env, stdin=subprocess.DEVNULL, stdout=stdout, stderr=stderr,
                                 creationflags=subprocess.CREATE_NO_WINDOW)
        try:
            sampler = ResourceSampler(child, output)
            while child.poll() is None:
                sampler.sample()
                if time.monotonic() - started > method['child_budget_seconds']:
                    raise ProbeFailure('child_budget_exceeded_no_retry')
                try:
                    child.wait(timeout=1)
                except subprocess.TimeoutExpired:
                    pass
        except (Exception, KeyboardInterrupt):
            interrupted = True
            stop = output / 'stop-request.json'
            if not stop.exists():
                write_new(stop, {'reason': 'outer_runner_stop'})
            try:
                child.wait(timeout=method['cooperative_stop_grace_seconds'])
            except subprocess.TimeoutExpired:
                # Exact child handle only. Without its ownership report Linux
                # cleanup remains unconfirmed, even after the test process exits.
                child.terminate()
                try:
                    child.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait(timeout=5)
                report['forced_child_termination'] = True
            raise
        finally:
            if sampler is not None:
                report['resources'] = sampler.close(output)
            report['child'] = {'exit_code': child.poll(), 'interrupted': interrupted,
                               'elapsed_ms': (time.monotonic() - started) * 1000}
    if child.returncode != 0:
        raise ProbeFailure('joint_child_failed_no_retry')
    inner = output / 'report.json'
    inner_report = json.loads(inner.read_text(encoding='utf-8'))
    check_report(inner_report)
    if method['method'] == FLASH_METHOD:
        setup = json.loads((output / 'setup.json').read_text(encoding='utf-8'))
        expected = setup['foreground_staging']['copied_sha256']['runtime/kimi/verification-rust.json']
        if inner_report['provider_preflight']['verification_receipt_sha256'] != expected:
            raise ProbeFailure('flash_kimi_verification_receipt_changed')
    report['child']['report_sha256'] = digest(inner)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--real-camera-local-synthetic', action='store_true', required=True)
    scenario = parser.add_mutually_exclusive_group()
    scenario.add_argument('--soak', action='store_true', help='Fixed 1800s managed bilingual soak; cap96, no retry')
    scenario.add_argument('--flash-joint', action='store_true', help='Two zh/en Flash voice rounds and two Kimi memory jobs; no retry')
    parser.add_argument('--root', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--test-executable', type=Path, required=True)
    parser.add_argument('--camera-worker', type=Path, required=True)
    parser.add_argument('--build-manifest', type=Path, required=True)
    parser.add_argument('--camera-id-sha256', required=True)
    parser.add_argument('--distribution', required=True)
    parser.add_argument('--final-asr-python', required=True)
    parser.add_argument('--final-asr-cache', required=True)
    args = parser.parse_args()
    root = args.root.resolve(strict=True)
    output = args.output.resolve()
    if not output.is_relative_to(root / 'runtime/goal-baselines'):
        raise ValueError('output_must_be_an_isolated_baseline_directory')
    executable = args.test_executable.resolve(strict=True)
    worker = args.camera_worker.resolve(strict=True)
    build_manifest = args.build_manifest.resolve(strict=True)
    camera = selected_camera(root, args.camera_id_sha256)
    output.mkdir(parents=True, exist_ok=False)
    report = {'test_type': 'managed_joint_runner', 'success': False,
              'model_service_owned_by_runner': False, 'forced_child_termination': False,
              'child_stdio_retained': False,
              'independent_semantic_review': 'pending', 'errors': [],
              'protected_before_sha256': protections(root)}
    try:
        method = stage(root, output, executable, worker, args.camera_id_sha256, args.distribution,
                       build_manifest, args.final_asr_python, args.final_asr_cache, args.soak, args.flash_joint)
        run_child(root, output, executable, camera, method, report)
        report['success'] = True
    except (Exception, KeyboardInterrupt) as error:
        # ProbeFailure messages are fixed local codes; dependency exception
        # details can contain paths or remote text and remain type-only.
        report['errors'].append(str(error) if isinstance(error, ProbeFailure) else type(error).__name__)
    finally:
        report['protected_after_sha256'] = protections(root)
        report['protected_files_unchanged'] = report['protected_after_sha256'] == report['protected_before_sha256']
        if not report['protected_files_unchanged']:
            report['success'] = False
        write_new(output / 'runner-report.json', report)
    print(json.dumps({'success': report['success'], 'output': str(output),
                      'semantic_review': 'pending', 'model_services_owned_by_runner': False}))
    return 0 if report['success'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
