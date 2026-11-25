"""Report acceptance must not turn a short/idle/unclean run into a full soak."""
import copy
import hashlib
import tempfile
import unittest
from pathlib import Path

from voice_loop_soak_model_smoke import ProbeFailure, check_soak, protected, selected_camera
from voice_loop_soak_resources import gpu_rows


def report():
    rounds = []
    for index in range(60):
        binding = {'request_id': f'request-{index}', 'generation': index + 1}
        rounds.append({'completed': True, 'rearm_observed': True, 'submit': binding,
                       'provider_diagnostics': {**binding, 'matched': True,
                                                'last_acp_segments': {**binding, 'success': True}}})
    metrics = {'requests': 60, 'acp_segment_requests': 60,
               'last_acp_segments': rounds[-1]['provider_diagnostics']['last_acp_segments']}
    return {
        'test_type': 'real_synthetic_voice_loop_camera_soak', 'passed': True,
        'diagnostics_revision': 2,
        'language': 'zh', 'duration_target_ms': 1800000, 'active_duration_ms': 1805000,
        'teaching_request_limit': 96, 'budget_exhausted_before_duration': False,
        **{k: True for k in ('production_capture_loop', 'synthetic_capture_source', 'production_audio_owner',
                            'simulated_display_ack', 'protected_files_unchanged', 'provider_cleanup_confirmed',
                            'camera_opened', 'metadata_only')},
        **{k: False for k in ('microphone_opened', 'ptz_enabled', 'raw_pcm_saved', 'speaker_output_measured',
                             'browser_tested', 'acoustic_loop_tested')},
        'memory_background': 'not_exercised',
        'owned_asr_stop': {k: True for k in ('cleanup_confirmed', 'child_exited', 'interpreter_exited',
                                          'launcher_job_empty', 'interpreter_job_empty', 'interpreter_job_owned')},
        'owned_camera_stop': {'cleanup_confirmed': True, 'supervisor_exited': True,
                              'child_started_count': 1, 'child_wait_confirmed_count': 1, 'jobs_empty_count': 1},
        'completed_rounds': 60,
        'rounds': rounds,
        'provider_metrics': copy.deepcopy(metrics),
        'provider_metrics_before_cleanup': copy.deepcopy(metrics),
        'provider_metrics_after_cleanup': copy.deepcopy(metrics),
        'source_lifecycle': {'opens': 60, 'closes': 60, 'active': False},
        'five_minute_windows': [{'index': i + 1, 'start_ms': i * 300000, 'end_ms': (i + 1) * 300000,
                                'covered_ms': 300000, 'rounds_completed': 10,
                                'accepted_processed_frames_delta': 7200, 'sampled_observations': 300}
                               for i in range(6)],
        'camera': {'metadata_only': True, 'epoch_changes': 0, 'samples_count': 1800,
                   'accepted_processed_frames': 43200, 'maximum_sample_gap_ms': 1030,
                   'tail_sample_gap_ms': 700, 'maximum_allowed_sample_gap_ms': 3000,
                   'observation_coverage_complete': True},
    }


class SoakContractTests(unittest.TestCase):
    def test_unavailable_or_invalid_gpu_values_are_not_replaced_by_zero(self):
        self.assertEqual(gpu_rows('8192, 12288, 50')[0]['memory_used_mib'], 8192)
        for text in ('', 'N/A, 12288, 0', 'nan, 12288, 0', '13000, 12288, 10', '10, 0, 0'):
            with self.assertRaises(ValueError):
                gpu_rows(text)

    def test_short_or_idle_window_never_counts_as_full_duration(self):
        check_soak(report())
        for field, value in [('active_duration_ms', 1799999), ('budget_exhausted_before_duration', True)]:
            changed = report()
            changed[field] = value
            with self.assertRaises(ProbeFailure):
                check_soak(changed)
        for field, value in [('covered_ms', 299999), ('rounds_completed', 0),
                             ('accepted_processed_frames_delta', 0), ('sampled_observations', 0)]:
            changed = report()
            changed['five_minute_windows'][4][field] = value
            with self.assertRaises(ProbeFailure):
                check_soak(changed)

    def test_restarted_camera_or_missing_owned_cleanup_is_not_accepted(self):
        for field in ('child_started_count', 'child_wait_confirmed_count', 'jobs_empty_count'):
            changed = report()
            changed['owned_camera_stop'][field] = 2
            with self.assertRaises(ProbeFailure):
                check_soak(changed)
        for field in ('cleanup_confirmed', 'supervisor_exited'):
            changed = report()
            changed['owned_camera_stop'][field] = False
            with self.assertRaises(ProbeFailure):
                check_soak(changed)
        changed = report()
        changed['owned_asr_stop']['interpreter_job_empty'] = False
        with self.assertRaises(ProbeFailure):
            check_soak(changed)

    def test_long_camera_observation_gap_cannot_be_hidden_by_a_fresh_final_frame(self):
        for key in ('maximum_sample_gap_ms', 'tail_sample_gap_ms'):
            for gap in (None, 3001, float('nan')):
                changed = report()
                changed['camera'][key] = gap
                with self.assertRaises(ProbeFailure):
                    check_soak(changed)

    def test_report_requires_completed_native_playback_rearm_and_exact_calls(self):
        for change in ('unfinished', 'missing_capture_close', 'extra_request', 'camera_epoch'):
            changed = copy.deepcopy(report())
            if change == 'unfinished':
                changed['rounds'][-1]['rearm_observed'] = False
            elif change == 'missing_capture_close':
                changed['source_lifecycle']['closes'] -= 1
            elif change == 'extra_request':
                changed['provider_metrics']['requests'] += 1
            else:
                changed['camera']['epoch_changes'] = 1
            with self.assertRaises(ProbeFailure):
                check_soak(changed)

    def test_only_explicit_saved_camera_identity_is_selected(self):
        with tempfile.TemporaryDirectory() as path:
            root = Path(path)
            (root / 'runtime').mkdir()
            selection = root / 'runtime/rust_camera_device.txt'
            selection.write_text('synthetic-camera-id\n', encoding='utf-8')
            sha = hashlib.sha256(b'synthetic-camera-id').hexdigest()
            self.assertEqual(selected_camera(root, sha), 'synthetic-camera-id')
            selection.write_text('another-camera-id', encoding='utf-8')
            with self.assertRaises(ProbeFailure):
                selected_camera(root, sha)

    def test_old_or_missing_provider_metrics_cannot_certify_a_new_round(self):
        for field, value in [('request_id', 'old-request'), ('generation', 0)]:
            changed = report()
            changed['rounds'][-1]['provider_diagnostics']['last_acp_segments'][field] = value
            with self.assertRaises(ProbeFailure):
                check_soak(changed)
        changed = report()
        changed['rounds'][-1]['provider_diagnostics'] = {'matched': False, 'last_acp_segments': None}
        with self.assertRaises(ProbeFailure):
            check_soak(changed)
        # Both observations are mandatory; a pre-cleanup snapshot is not proof
        # that the final provider cancellation/cleanup record was observed.
        changed = report()
        del changed['provider_metrics_after_cleanup']
        with self.assertRaises(ProbeFailure):
            check_soak(changed)

    def test_runtime_agent_and_teaching_policy_are_both_protected(self):
        with tempfile.TemporaryDirectory() as path:
            root = Path(path)
            (root / 'prompts').mkdir()
            core = root / 'prompts/tutor_core.md'
            agent = root / 'prompts/tutor-agent.md'
            core.write_text('synthetic core', encoding='utf-8')
            agent.write_text('synthetic agent', encoding='utf-8')
            before = protected(root)
            agent.write_text('changed agent', encoding='utf-8')
            after = protected(root)
            self.assertNotEqual(before['prompts/tutor-agent.md'], after['prompts/tutor-agent.md'])
            self.assertEqual(before['prompts/tutor_core.md'], after['prompts/tutor_core.md'])
            core.write_text('changed core', encoding='utf-8')
            self.assertNotEqual(after['prompts/tutor_core.md'], protected(root)['prompts/tutor_core.md'])


if __name__ == '__main__':
    unittest.main()
