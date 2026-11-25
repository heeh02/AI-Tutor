"""Launcher lifecycle checks with fake descriptors and no real services."""
import contextlib
import importlib.util
import io
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import Mock, patch


spec = importlib.util.spec_from_file_location('robot_demo_launch', Path(__file__).with_name('launch.py'))
launch = importlib.util.module_from_spec(spec)
spec.loader.exec_module(launch)


class LauncherTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / 'runtime').mkdir()
        self.path = self.root / 'runtime/server.json'
        # A stand-in file only: these tests never read a local credential.
        self.path.touch()

    def arguments(self, *args):
        return patch('sys.argv', ['launch.py', '--root', str(self.root), *args])

    def test_stop_requests_native_shutdown_even_when_state_is_unresponsive(self):
        def stop(*args, **kwargs):
            self.path.unlink()
            return subprocess.CompletedProcess(args[0], 0)

        with self.arguments('--stop'), \
                patch.object(launch, 'descriptor', return_value=None) as health, \
                patch.object(launch.subprocess, 'run', side_effect=stop) as native:
            self.assertEqual(launch.main(), 0)
        health.assert_not_called()
        self.assertEqual(native.call_args.args[0], [str(self.root / 'bin/ai-tutor.exe'), '--stop', '--root', str(self.root)])
        self.assertTrue(native.call_args.kwargs['check'])

    def test_stop_is_already_complete_when_no_descriptor_exists(self):
        self.path.unlink()
        with self.arguments('--stop'), patch.object(launch.subprocess, 'run') as native:
            self.assertEqual(launch.main(), 0)
        native.assert_not_called()

    def test_native_stop_failure_is_not_reported_as_success(self):
        with self.arguments('--stop'), \
                patch.object(launch.subprocess, 'run', side_effect=subprocess.CalledProcessError(1, 'fake-stop')):
            with self.assertRaises(subprocess.CalledProcessError):
                launch.main()
        self.assertTrue(self.path.exists())

    def test_stop_waits_for_descriptor_removal_and_reports_timeout(self):
        with self.arguments('--stop'), \
                patch.object(launch.subprocess, 'run'), \
                patch.object(launch.time, 'monotonic', side_effect=[0, 121]):
            with self.assertRaisesRegex(RuntimeError, '仍在关闭'):
                launch.main()
        self.assertTrue(self.path.exists())

    def test_exited_concurrent_launcher_waits_for_winning_service(self):
        ready = {'port': 8879, 'token': 'test-only-placeholder', 'pid': 123}
        exited = Mock()
        exited.poll.return_value = 1
        output = io.StringIO()
        with self.arguments('--no-open'), \
                patch.object(launch, 'descriptor', side_effect=[None, None, None, ready]) as health, \
                patch.object(launch.subprocess, 'Popen', return_value=exited) as spawn, \
                patch.object(launch.time, 'monotonic', side_effect=[0, 1, 2, 3]), \
                patch.object(launch.time, 'sleep'), contextlib.redirect_stdout(output):
            self.assertEqual(launch.main(), 0)
        self.assertEqual(health.call_count, 4)
        spawn.assert_called_once()
        self.assertIn('"running": true', output.getvalue())
        self.assertNotIn(ready['token'], output.getvalue())

    def test_startup_still_has_a_bounded_failure_deadline(self):
        with self.arguments('--no-open'), \
                patch.object(launch, 'descriptor', return_value=None), \
                patch.object(launch.subprocess, 'Popen'), \
                patch.object(launch.time, 'monotonic', side_effect=[0, 36]), \
                patch.object(launch.time, 'sleep') as sleep:
            with self.assertRaisesRegex(RuntimeError, '启动失败'):
                launch.main()
        sleep.assert_not_called()

    def test_running_service_is_reused_without_spawning(self):
        ready = {'port': 8879, 'token': 'test-only-placeholder', 'pid': 123}
        with self.arguments('--no-open'), \
                patch.object(launch, 'descriptor', return_value=ready), \
                patch.object(launch.subprocess, 'Popen') as spawn, \
                contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(launch.main(), 0)
        spawn.assert_not_called()


if __name__ == '__main__':
    unittest.main()
