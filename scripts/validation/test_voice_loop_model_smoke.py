"""Reject partial or legacy transport evidence as a complete voice-loop result."""
import copy
import unittest

from voice_loop_model_smoke import ProbeFailure, check_voice_loop


def completed_fixture():
    return {
        'test_type': 'real_synthetic_voice_loop_two_rounds', 'language': 'zh', 'passed': True,
        **{key: True for key in ('production_capture_loop', 'synthetic_capture_source',
                                'production_audio_owner', 'simulated_display_ack',
                                'protected_files_unchanged', 'provider_cleanup_confirmed')},
        **{key: False for key in ('microphone_opened', 'camera_opened', 'ptz_enabled',
                                 'raw_pcm_saved', 'speaker_output_measured')},
        'owned_asr_stop': {key: True for key in (
            'cleanup_confirmed', 'child_exited', 'interpreter_exited',
            'launcher_job_empty', 'interpreter_job_empty', 'interpreter_job_owned')},
        'provider_metrics': {'requests': 2, 'acp_segment_requests': 2},
        'source_lifecycle': {'opens': 2, 'closes': 2},
        'rounds': [{'case': 'round_1', 'completed': True, 'rearm_observed': True},
                   {'case': 'round_2', 'completed': True, 'rearm_observed': True}],
    }


class LoopReportTests(unittest.TestCase):
    def test_both_completed_rounds_and_closed_input_are_required(self):
        check_voice_loop(completed_fixture(), 'zh')
        candidates = []
        candidate = completed_fixture()
        candidate['rounds'][1]['rearm_observed'] = False
        candidates.append(candidate)
        candidate = completed_fixture()
        candidate['source_lifecycle']['closes'] = 1
        candidates.append(candidate)
        candidate = completed_fixture()
        candidate['owned_asr_stop']['interpreter_job_empty'] = False
        candidates.append(candidate)
        for candidate in candidates:
            with self.assertRaises(ProbeFailure):
                check_voice_loop(candidate, 'zh')

    def test_fake_speech_or_old_transport_report_cannot_pass(self):
        for key, value in [('production_audio_owner', False),
                           ('production_capture_loop', False),
                           ('test_type', 'real_synthetic_production_segment_playback')]:
            candidate = completed_fixture()
            candidate[key] = value
            with self.assertRaises(ProbeFailure):
                check_voice_loop(candidate, 'zh')

    def test_unreported_retry_or_broader_claim_cannot_pass(self):
        baseline = completed_fixture()
        for path, value in [(('provider_metrics', 'requests'), 3),
                             (('microphone_opened',), True),
                             (('speaker_output_measured',), True)]:
            candidate = copy.deepcopy(baseline)
            node = candidate
            for key in path[:-1]:
                node = node[key]
            node[path[-1]] = value
            with self.assertRaises(ProbeFailure):
                check_voice_loop(candidate, 'zh')
        with self.assertRaises(ProbeFailure):
            check_voice_loop(baseline, 'en')


if __name__ == '__main__':
    unittest.main()
