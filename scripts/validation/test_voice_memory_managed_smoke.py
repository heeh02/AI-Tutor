"""Offline acceptance checks only; these tests never start models or devices."""
import copy
import json
from pathlib import Path
import tempfile
import unittest

from voice_memory_managed_smoke import (
    BUNDLE, CPU_BUNDLE, FINAL_BUNDLE, FINAL_PROVIDER, FLASH_ARTIFACTS, FLASH_METHOD,
    METHOD, SOAK_METHOD, SOURCES, ProbeFailure, check_report, digest,
    select_method, stage_flash_provider, verified_build_binding,
)


def report():
    recovery = {'same_job_recovered': True, 'substantive_type_source_coverage': True,
                'job': {'attempts': 2, 'second_lease_differs': True, 'status': 'shadow_ready',
                        'candidates': [{'status': 'shadow'}]}}
    job = {'preemption': {'covered': True, 'observed_pending_without_candidates': True},
           'recovery': recovery}
    return {'test_type': 'managed_workstation_camera_voice_shadow', 'passed': True,
            'method': {'method': METHOD}, 'expected_asr_provider': FINAL_PROVIDER,
            **{name: True for name in ('operation_completed', 'cleanup_confirmed',
               'scheduler_join_confirmed', 'provider_cleanup_confirmed', 'protected_files_unchanged',
               'heartbeat_finished_cleanly', 'camera_sampler_finished_cleanly',
               'final_asr_cleanup_confirmed', 'gpu_pair_observed_active_at_completion')},
            **{name: False for name in ('microphone_opened', 'ptz_enabled', 'speaker_output_measured',
               'remote_generation_inflight_preemption_proven',
               'raw_media_saved', 'release_main_startup_tested', 'thirty_minute_soak',
               'browser_tested', 'acoustic_loop_tested')},
            'rounds': [{'completed': True, 'passed': True, 'submitted_actual_asr_text_unchanged': True,
                        'expected_asr_provider': FINAL_PROVIDER,
                        'asr': {'provider': FINAL_PROVIDER, 'fallback_reason': None}}
                       for _ in range(2)],
            'final_asr_after_cleanup': {'cleanup_confirmed': True, 'active': False, 'busy': False,
                'ownership_stop': {'cleanup_confirmed': True, 'linux_cleanup_confirmed': True,
                    'protocol_failed': False, 'normal_owner_stop': True, 'bridge_exit_code': 0,
                    'receipt': {'state': 'owned_stopped', 'linux_cleanup_confirmed': True,
                                'process_started': True, 'service_exit_code': 0,
                                'forced_group_stop': False}}},
            'jobs': {'A': copy.deepcopy(job), 'B': copy.deepcopy(job)},
            'memory_counts': {'jobs': 2, 'reservations': 4, 'starts': 4, 'background_memories': 0},
            'final_memory_audit': {'namespace': 'simulator', 'all_statuses_included': True,
                 'voice_learner_id': 'voice-learner', 'voice_learner_memory_count': 0,
                 'whole_database_memory_count': 0, 'records': []},
            'camera': {'observation_coverage_complete': True}, 'independent_semantic_review': 'pending'}


def soak_report():
    value = report()
    value['method'] = {'method': SOAK_METHOD}
    value['thirty_minute_soak'] = True
    value['active_duration_ms'] = 1800100
    value['soak'] = {'active_window_reached': True, 'minimum_active_ms': 1800000,
        'foreground_limit': 96, 'idle_padding': False, 'late_b_recovered': True,
        'bilingual_phases_completed': True}
    value['jobs']['A']['started_active_ms'] = 15000
    value['jobs']['B']['started_active_ms'] = 1515000
    value['language_transition'] = {'completed': True, 'from': 'zh', 'to': 'en',
        'elapsed_ms': 100, 'at_active_ms': 900100,
        'normal_settings_and_enable_apis': True, 'camera_reopened': False}
    template = value['rounds'][0]
    value['rounds'] = []
    counts = {'zh': 0, 'en': 0}
    for index in range(1, 90):
        language = 'zh' if index <= 44 else 'en'
        counts[language] += 1
        row = copy.deepcopy(template)
        row.update(case='round_' + str(index), language=language,
            source_id=language + '_short',
            source_sha256=('ad72efd95a1abc540737554f9ccb63b7a9933effb75e5e458824a90363bb9a33'
                if language == 'zh' else '670191397605e3f21f2b0e331852b665f91e5ee21cb9573f2ffdd93aff825a9f'),
            source_round_index=counts[language], user_turn_total=index,
            source_lifecycle={'opens': counts[language], 'closes': counts[language]},
            protected_files_unchanged=True, gpu_pair_active_same_tts_instance=True,
            whisper_fallback_used=False, soak_started_ms=index * 20000,
            soak_finished_ms=(index + 1) * 20000,
            shadow_preemption='A' if index == 1 else ('B' if index == 75 else None),
            memory_audit=copy.deepcopy(value['final_memory_audit']))
        value['rounds'].append(row)
    value['source_lifecycles'] = [{'active': False, 'opens': count, 'closes': count,
        'closed_streams': [{'producer_joined': True} for _ in range(count)]}
        for count in counts.values()]
    return value


def flash_report():
    value = report()
    value.update(method={'method': FLASH_METHOD}, foreground_provider='deepseek_flash',
                 language='zh_then_en', automatic_teaching_retry=False,
                 final_asr_instance_id='synthetic-final-asr-owner')
    value['provider_preflight'] = {
        'foreground': {'provider': 'deepseek_flash', 'available': True, 'thinking': 'non_thinking',
            'stream_contract': 'metadata_v2', 'reply_text_origin': 'server_segment_concatenation',
            'readiness_basis': 'local_client_and_credential_preflight_only',
            'remote_verified_by_status_poll': False},
        'kimi': {'available': True, 'verified': True, 'tools': []},
        'verification_receipt_sha256': 'a' * 64}
    value['language_transition'] = {'completed': True, 'from': 'zh', 'to': 'en',
        'elapsed_ms': 100, 'normal_settings_and_enable_apis': True, 'camera_reopened': False}
    for index, row in enumerate(value['rounds'], 1):
        language = 'zh' if index == 1 else 'en'
        row.update(case='round_' + str(index), language=language, source_id=language + '_short',
            source_sha256=('ad72efd95a1abc540737554f9ccb63b7a9933effb75e5e458824a90363bb9a33'
                if language == 'zh' else '670191397605e3f21f2b0e331852b665f91e5ee21cb9573f2ffdd93aff825a9f'),
            source_round_index=1, user_turn_total=index,
            source_lifecycle={'opens': 1, 'closes': 1}, protected_files_unchanged=True,
            gpu_pair_active_same_tts_instance=True, whisper_fallback_used=False,
            final_asr_instance_id='synthetic-final-asr-owner', same_final_asr_instance=True,
            shadow_preemption='A' if index == 1 else 'B',
            memory_audit=copy.deepcopy(value['final_memory_audit']),
            submit={'request_id': 'request-' + str(index), 'generation': index})
        row['provider_diagnostics'] = {
            **row['submit'], 'matched': True,
            'last_foreground_segments': {**row['submit'], 'transport': 'deepseek_http',
                'stream_contract': 'metadata_v2', 'reply_text_origin': 'server_segment_concatenation',
                'success': True, 'operation_success': True}}
    for key in ('provider_before_cleanup', 'provider_after_cleanup'):
        value[key] = {'requests': 6, 'deepseek_segment_requests': 2,
                      'consolidation_requests': 4, 'retries': 0}
    value['source_lifecycles'] = [{'active': False, 'opens': 1, 'closes': 1,
        'closed_streams': [{'producer_joined': True}]} for _ in range(2)]
    return value


class AcceptanceTests(unittest.TestCase):
    def test_flash_cannot_accept_wrong_request_transport_extra_calls_or_stale_input(self):
        check_report(flash_report())
        for mutate in (
            lambda v: v['rounds'][1]['provider_diagnostics']['last_foreground_segments'].update(request_id='old'),
            lambda v: v['rounds'][0]['provider_diagnostics']['last_foreground_segments'].update(transport='acp_segments_v1'),
            lambda v: v['rounds'][1]['provider_diagnostics'].update(matched=False),
            lambda v: v['provider_before_cleanup'].update(requests=7, deepseek_segment_requests=3),
            lambda v: v['provider_after_cleanup'].update(retries=1),
            lambda v: v['provider_after_cleanup'].update(acp_segment_requests=1),
            lambda v: v['jobs']['A']['recovery']['job'].update(attempts=3),
            lambda v: v['language_transition'].update(completed=False),
            lambda v: v['rounds'][1].update(source_round_index=2),
            lambda v: v['rounds'][1].update(final_asr_instance_id='a-different-owner'),
            lambda v: v['source_lifecycles'][1].update(closed_streams=[]),
            lambda v: v['provider_preflight']['foreground'].update(remote_verified_by_status_poll=True),
            lambda v: v.update(cleanup_confirmed=False),
        ):
            value = flash_report()
            mutate(value)
            with self.assertRaises(ProbeFailure):
                check_report(value)

    def test_flash_selection_preserves_existing_modes_and_rejects_soak(self):
        self.assertEqual(select_method()[0], METHOD)
        self.assertEqual(select_method(soak=True)[0], SOAK_METHOD)
        self.assertEqual(select_method(flash_joint=True)[0], FLASH_METHOD)
        with self.assertRaises(ProbeFailure):
            select_method(soak=True, flash_joint=True)

    def test_flash_staging_copies_only_reviewed_nonsecret_inputs(self):
        with tempfile.TemporaryDirectory() as directory:
            root, fixture = Path(directory) / 'root', Path(directory) / 'fixture'
            for name in FLASH_ARTIFACTS:
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text('synthetic nonsecret input', encoding='utf-8')
            original_policy = root / 'runtime/teaching-provider-policy.json'
            original_policy.write_text('unchanged original policy', encoding='utf-8')
            (root / '.env').write_text('synthetic private marker', encoding='utf-8')
            result = stage_flash_provider(root, fixture)
            self.assertFalse(result['credential_copied'])
            self.assertEqual(set(result['copied_sha256']), set(FLASH_ARTIFACTS))
            self.assertEqual(original_policy.read_text(encoding='utf-8'), 'unchanged original policy')
            self.assertFalse((fixture / '.env').exists())
            self.assertEqual(json.loads((fixture / 'runtime/teaching-provider-policy.json').read_text())['provider'],
                             'deepseek_flash')
            for name, expected in result['copied_sha256'].items():
                self.assertEqual(digest(fixture / name), expected)

    def test_soak_requires_real_duration_late_memory_and_round_evidence(self):
        check_report(soak_report())
        for mutate in (
            lambda v: v.update(active_duration_ms=1799999),
            lambda v: v.update(active_duration_ms=2100001),
            lambda v: v['soak'].update(idle_padding=True),
            lambda v: v['jobs']['B'].update(started_active_ms=1400000),
            lambda v: v['rounds'][50]['asr'].update(provider='fallback'),
            lambda v: v['rounds'][50].update(gpu_pair_active_same_tts_instance=False),
            lambda v: v['rounds'][50].update(memory_audit={}),
            lambda v: v['language_transition'].update(elapsed_ms=5001),
            lambda v: v['source_lifecycles'][1].update(closed_streams=[]),
        ):
            value = soak_report()
            mutate(value)
            with self.assertRaises(ProbeFailure):
                check_report(value)

    def test_soak_cannot_turn_short_rounds_plus_idle_into_thirty_minutes(self):
        value = soak_report()
        value['rounds'][20]['soak_started_ms'] += 15000
        with self.assertRaisesRegex(ProbeFailure, 'soak_unexplained_workload_gap'):
            check_report(value)

    def test_bundle_is_bound_to_the_declared_build_and_exact_executables(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            required = {name for name in SOURCES if not name.endswith('.py')}
            required.update('scripts/speech-lab/' + name for name in BUNDLE)
            required.update('scripts/final-asr/' + name for name in FINAL_BUNDLE)
            required.update('scripts/speech-service/' + name for name in CPU_BUNDLE)
            self.assertIn('rust/ai-tutor/src/gpu_queue.rs', SOURCES)
            for name in required:
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(b'current measured source')
            executable, worker = root / 'test.exe', root / 'worker.exe'
            executable.write_bytes(b'measured test')
            worker.write_bytes(b'measured worker')
            manifest = root / 'build.json'
            measured = {'source_files_sha256': {name: digest(root / name) for name in required},
                        'executables': [{'name': path.name, 'sha256': digest(path)} for path in (executable, worker)]}
            manifest.write_text(json.dumps(measured), encoding='utf-8')
            binding = verified_build_binding(root, manifest, executable, worker)
            self.assertEqual(binding['bundle_sha256']['managed_tts.py'], digest(root / 'scripts/speech-lab/managed_tts.py'))
            self.assertEqual(binding['bundle_sha256']['gpu_admission.py'], digest(root / 'scripts/speech-lab/gpu_admission.py'))
            self.assertEqual(binding['final_bundle_sha256']['qwen_final_engine.py'], digest(root / 'scripts/final-asr/qwen_final_engine.py'))
            self.assertEqual(binding['cpu_bundle_sha256']['recognizer.py'], digest(root / 'scripts/speech-service/recognizer.py'))
            # A changed runtime helper must not be accepted merely because some
            # older successful process used a similarly named bundle.
            helper = root / 'scripts/speech-lab/managed_tts.py'
            helper.write_bytes(b'unmeasured helper')
            with self.assertRaisesRegex(ProbeFailure, 'measured_runtime_source_mismatch'):
                verified_build_binding(root, manifest, executable, worker)
            helper.write_bytes(b'current measured source')
            for name in ('rust/ai-tutor/src/gpu_queue.rs', 'scripts/speech-lab/gpu_admission.py',
                         'scripts/final-asr/qwen_final_engine.py', 'scripts/final-asr/qwen-model.json',
                         'scripts/speech-service/recognizer.py'):
                with self.subTest(unmeasured_source=name):
                    path = root / name
                    path.write_bytes(b'unmeasured runtime change')
                    with self.assertRaisesRegex(ProbeFailure, 'measured_runtime_source_mismatch'):
                        verified_build_binding(root, manifest, executable, worker)
                    path.write_bytes(b'current measured source')
            missing_queue = copy.deepcopy(measured)
            del missing_queue['source_files_sha256']['rust/ai-tutor/src/gpu_queue.rs']
            manifest.write_text(json.dumps(missing_queue), encoding='utf-8')
            with self.assertRaisesRegex(ProbeFailure, 'measured_runtime_source_mismatch'):
                verified_build_binding(root, manifest, executable, worker)
            manifest.write_text(json.dumps(measured), encoding='utf-8')
            worker.write_bytes(b'unmeasured worker')
            with self.assertRaisesRegex(ProbeFailure, 'measured_executable_mismatch'):
                verified_build_binding(root, manifest, executable, worker)

    def test_captured_qwen_final_cannot_be_replaced_by_wire_label_or_fallback(self):
        for provider in ('sherpa-onnx / bilingual Zipformer / local streaming',
                         'Qwen3-ASR-1.7B / PyTorch BF16 / local offline final'):
            with self.subTest(provider=provider):
                value = report()
                value['rounds'][0]['asr']['provider'] = provider
                with self.assertRaisesRegex(ProbeFailure, 'two_foreground_rounds_not_complete'):
                    check_report(value)
        value = report()
        value['rounds'][1]['asr']['fallback_reason'] = 'precise service unavailable'
        with self.assertRaisesRegex(ProbeFailure, 'two_foreground_rounds_not_complete'):
            check_report(value)

    def test_normal_owner_stop_does_not_hide_protocol_failure_or_busy_runtime(self):
        for field, bad_value in (('protocol_failed', True), ('protocol_failed', None),
                                 ('busy', True), ('busy', None)):
            with self.subTest(field=field, value=bad_value):
                value = report()
                stopped = value['final_asr_after_cleanup']
                target = stopped if field == 'busy' else stopped['ownership_stop']
                if bad_value is None:
                    del target[field]
                else:
                    target[field] = bad_value
                # Leave passed/normal_owner_stop/cleanup_confirmed true: those
                # aggregate flags must not mask the contradictory owned state.
                with self.assertRaisesRegex(ProbeFailure, 'final_asr_owned_cleanup_incomplete'):
                    check_report(value)

    def test_final_cleanup_needs_service_and_bridge_completion(self):
        for field, bad_value in (('process_started', False), ('service_exit_code', 1),
                                 ('forced_group_stop', True), ('linux_cleanup_confirmed', False)):
            with self.subTest(field=field):
                value = report()
                value['final_asr_after_cleanup']['ownership_stop']['receipt'][field] = bad_value
                with self.assertRaisesRegex(ProbeFailure, 'final_asr_owned_cleanup_incomplete'):
                    check_report(value)
        value = report()
        value['final_asr_after_cleanup']['ownership_stop']['bridge_exit_code'] = 1
        with self.assertRaisesRegex(ProbeFailure, 'final_asr_owned_cleanup_incomplete'):
            check_report(value)

    def test_foreground_memory_is_retained_for_review_without_forcing_zero(self):
        value = report()
        value['final_memory_audit'].update(voice_learner_memory_count=1, whole_database_memory_count=1,
            records=[{'learner_id': 'voice-learner', 'content': 'A synthetic pending plan, not a result.',
                      'provenance_links': [], 'current_sources': [], 'missing_or_inactive_source_ids': []}])
        check_report(value)
        del value['final_memory_audit']['records'][0]['current_sources']
        with self.assertRaises(ProbeFailure):
            check_report(value)

    def test_empty_or_unpreempted_background_is_not_a_success(self):
        check_report(report())
        for mutate in (
            lambda value: value['jobs']['A']['recovery']['job'].update(candidates=[], status='completed'),
            lambda value: value['jobs']['B']['preemption'].update(covered=False),
            lambda value: value['jobs']['A']['recovery']['job'].update(second_lease_differs=False),
        ):
            value = report()
            mutate(value)
            with self.assertRaises(ProbeFailure):
                check_report(value)

    def test_budget_cleanup_and_review_scope_are_separate_gates(self):
        for mutate in (
            lambda value: value['memory_counts'].update(reservations=5),
            lambda value: value['memory_counts'].update(background_memories=1),
            lambda value: value.update(scheduler_join_confirmed=False),
            lambda value: value.update(independent_semantic_review='passed'),
        ):
            value = report()
            mutate(value)
            with self.assertRaises(ProbeFailure):
                check_report(value)


if __name__ == '__main__':
    unittest.main()
