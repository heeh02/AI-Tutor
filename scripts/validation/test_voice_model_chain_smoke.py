"""Report acceptance must distinguish native callbacks from transport-only data."""
import copy
import unittest

from voice_model_chain_smoke import ProbeFailure, check_playback


def report():
    return {
        'test_type': 'real_synthetic_production_segment_playback', 'language': 'zh',
        'passed': True, 'production_audio_owner': True, 'simulated_display_ack': True,
        'speaker_output_measured': False, 'protected_files_unchanged': True,
        'microphone_opened': False, 'camera_opened': False, 'ptz_enabled': False,
        'raw_pcm_saved': False, 'overlap_observed': False,
        'owned_asr_stop': {key: True for key in (
            'cleanup_confirmed', 'child_exited', 'interpreter_exited',
            'launcher_job_empty', 'interpreter_job_empty', 'interpreter_job_owned')},
        'asr': {'completed': True, 'fallback_requested': False},
        'provider_metrics': {'requests': 2, 'acp_segment_requests': 2},
        'rounds': [{'case': 'stop_then_late_ack', 'passed': True},
                   {'case': 'restart_complete', 'passed': True}],
    }


class PlaybackReportTests(unittest.TestCase):
    def test_complete_native_path_does_not_require_manufactured_overlap(self):
        check_playback(report(), 'zh')

    def test_transport_only_or_claimed_acoustics_cannot_pass_native_contract(self):
        for field, value in [('production_audio_owner', False),
                             ('speaker_output_measured', True),
                             ('simulated_display_ack', False),
                             ('microphone_opened', True), ('ptz_enabled', True)]:
            with self.subTest(field=field):
                candidate = report()
                candidate[field] = value
                with self.assertRaises(ProbeFailure):
                    check_playback(candidate, 'zh')

    def test_incomplete_owner_or_second_round_is_not_success(self):
        base = report()
        for path in [('owned_asr_stop', 'interpreter_job_empty'), ('asr', 'completed')]:
            candidate = copy.deepcopy(base)
            candidate[path[0]][path[1]] = False
            with self.assertRaises(ProbeFailure):
                check_playback(candidate, 'zh')
        candidate = copy.deepcopy(base)
        candidate['rounds'][1]['passed'] = False
        with self.assertRaises(ProbeFailure):
            check_playback(candidate, 'zh')
        candidate['rounds'] = candidate['rounds'][:1]
        with self.assertRaises(ProbeFailure):
            check_playback(candidate, 'zh')

    def test_extra_model_retry_or_language_mix_is_rejected(self):
        candidate = report()
        candidate['provider_metrics']['requests'] = 3
        with self.assertRaises(ProbeFailure):
            check_playback(candidate, 'zh')
        with self.assertRaises(ProbeFailure):
            check_playback(report(), 'en')


if __name__ == '__main__':
    unittest.main()
