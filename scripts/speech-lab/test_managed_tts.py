"""Linux process ownership tests with sleeping children, no model or device."""
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

import managed_tts as owner


def sleeper(ignore_term=False):
    code = "import signal,time; "
    if ignore_term:
        code += "signal.signal(signal.SIGTERM,signal.SIG_IGN); "
    code += "print('ready',flush=True); time.sleep(60)"
    child = subprocess.Popen([sys.executable, "-c", code], start_new_session=True,
                             stdout=subprocess.PIPE, text=True)
    assert child.stdout.readline().strip() == "ready"
    return child


def live(pid):
    try:
        return Path(f"/proc/{pid}/stat").read_text().split(") ", 1)[1][0] != "Z"
    except FileNotFoundError:
        return False


def free_ports():
    with socket.socket() as first, socket.socket() as second:
        first.bind(("127.0.0.1", 0))
        second.bind(("127.0.0.1", 0))
        return first.getsockname()[1], second.getsockname()[1]


def lab_shell(path, ports, inherited_fd=None, sleep=True):
    """Source the real lease helper, then exec only a sleeping Python fixture."""
    script = ('set -euo pipefail; source "$1"; '
              'tutor_tts_gpu_lease "$2" "$3" "$4" "$5"; '
              'exec "$2" -c "$6"')
    code = "import time; print('ready',flush=True)"
    if sleep:
        code += "; time.sleep(60)"
    environment = os.environ.copy()
    for name in owner.gpu_admission.ENV_NAMES:
        environment.pop(name, None)
    environment.pop("AI_TUTOR_TTS_LEASE_FD", None)
    if inherited_fd is not None:
        environment["AI_TUTOR_TTS_LEASE_FD"] = str(inherited_fd)
    return subprocess.Popen(
        ["bash", "-c", script, "lease-test", str(Path(__file__).with_name("tts_gpu_lease.sh").resolve()),
         sys.executable, str(path), *map(str, ports), code],
        env=environment, pass_fds=() if inherited_fd is None else (inherited_fd,),
        start_new_session=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)


@unittest.skipUnless(sys.platform == 'linux', 'Linux process and flock fixtures')
class OwnershipTests(unittest.TestCase):
    def setUp(self):
        policy = mock.patch.object(owner.gpu_admission, 'load_policy', return_value=owner.gpu_admission.EXCLUSIVE)
        policy.start()
        self.addCleanup(policy.stop)

    def test_installed_wrapper_uses_sibling_launcher_and_explicit_project_root(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            lease = owner.gpu_lease(root / "gpu.lock")
            commands = mock.Mock()
            commands.read.return_value = {"protocol_version": 1,
                                          "bearer_token": "synthetic_test_credential_0000000000"}
            process = mock.Mock(pid=999999999)
            with mock.patch.object(sys, "argv", ["managed_tts", "--root", str(root),
                                                  "--engine", "cosy", "--instance", "0" * 32]), \
                    mock.patch.object(owner, "Commands", return_value=commands), \
                    mock.patch.object(owner.signal, "signal"), \
                    mock.patch.object(owner, "gpu_lease", return_value=lease), \
                    mock.patch.object(owner, "check_ports_idle"), \
                    mock.patch.object(owner.subprocess, "Popen", return_value=process) as launch, \
                    mock.patch.object(owner, "supervise", return_value={"reason": "owner_stop",
                                      "forced_group_stop": False, "service_exit_code": -15}), \
                    mock.patch.object(owner, "emit"):
                self.assertEqual(owner.main(), 0)
                self.assertEqual(launch.call_args.args[0],
                                 ["bash", str(Path(owner.__file__).resolve().parent / "start_cosy_omni.sh")])
                self.assertEqual(launch.call_args.kwargs["cwd"], root)
                self.assertEqual(launch.call_args.kwargs["env"]["AI_TUTOR_PROJECT_ROOT"], str(root))

    def test_relocated_launchers_use_project_dependencies_and_sibling_assets(self):
        # Execute the real launchers with fake helpers and an argv-printing vllm;
        # no CUDA, shared GPU lease, model or network is initialized.
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            project, bundle, environment = base / "project with spaces", base / "installed bundle", base / "environment with spaces"
            bundle.mkdir()
            (environment / "bin").mkdir(parents=True)
            pointer = project / "runtime/speech-gpu/omni-env-path.txt"
            pointer.parent.mkdir(parents=True)
            pointer.write_text(str(environment) + "\n")
            (bundle / "tts_gpu_lease.sh").write_text("tutor_tts_gpu_lease() { :; }\n")
            (bundle / "cuda_link_env.sh").write_text(":\n")
            fake = environment / "bin/vllm"
            fake.write_text(f"#!{sys.executable}\nimport json,sys\nprint(json.dumps(sys.argv[1:]))\n")
            fake.chmod(0o700)
            variables = os.environ.copy()
            variables["AI_TUTOR_PROJECT_ROOT"] = str(project)
            variables["AI_TUTOR_MANAGED_TTS_TOKEN"] = "synthetic-no-network-credential"
            for engine, model, config in (
                    ("qwen", "Qwen3-TTS-12Hz-0.6B-CustomVoice", "qwen3_tts_12gb.yaml"),
                    ("cosy", "Fun-CosyVoice3-0.5B-2512/omni-overlay", "cosyvoice3_12gb_candidate.yaml")):
                name = f"start_{engine}_omni.sh"
                shutil.copyfile(Path(owner.__file__).with_name(name), bundle / name)
                result = subprocess.run(["bash", str(bundle / name)], env=variables,
                                        capture_output=True, text=True, check=True, timeout=5)
                arguments = json.loads(result.stdout)
                self.assertEqual(arguments[:2], ["serve", str(project / "models/speech" / model)])
                self.assertEqual(arguments[arguments.index("--deploy-config") + 1], str(bundle / config))
                self.assertNotIn(variables["AI_TUTOR_MANAGED_TTS_TOKEN"], result.stdout)
                self.assertFalse((project / "runtime/speech-cosy/api-key").exists())
                self.assertFalse((project / "runtime/speech-gpu/omni-api-key").exists())

    def test_split_control_frame_is_nonblocking_and_bounded(self):
        read, write = os.pipe()
        commands = owner.Commands(read)
        try:
            os.write(write, b'{"op":')
            self.assertIsNone(commands.read(0))
            os.write(write, b'"stop","instance_id":"x"}\n')
            self.assertEqual(commands.read(0), {"op": "stop", "instance_id": "x"})
            os.write(write, b"x" * 4097)
            with self.assertRaises(ValueError):
                commands.read(0)
        finally:
            commands.close()
            os.close(read)
            os.close(write)

    def test_child_keeps_gpu_lease_after_owner_fd_closes(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "gpu.lock"
            lease = owner.gpu_lease(path)
            child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"],
                                     start_new_session=True, pass_fds=(lease,))
            os.close(lease)
            try:
                with self.assertRaises(BlockingIOError):
                    owner.gpu_lease(path)
            finally:
                owner.stop_owned(child, .2)
            lease = owner.gpu_lease(path)
            os.close(lease)

    def test_foreign_listening_port_is_rejected_without_contact(self):
        with socket.socket() as server:
            server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            server.bind(("127.0.0.1", 0))
            server.listen(1)
            with self.assertRaises(RuntimeError):
                owner.check_ports_idle([server.getsockname()[1]])
            server.settimeout(.05)
            with self.assertRaises(TimeoutError):
                server.accept()

    def test_completed_tcp_connection_does_not_block_managed_or_lab_restart(self):
        with socket.socket() as server:
            server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            server.bind(("127.0.0.1", 0))
            server.listen(1)
            port = server.getsockname()[1]
            with socket.create_connection(("127.0.0.1", port)) as client:
                peer, _ = server.accept()
                with peer:
                    peer.shutdown(socket.SHUT_WR)  # The server owns TIME_WAIT.
                    self.assertEqual(client.recv(1), b"")
        with socket.socket() as old_probe:
            with self.assertRaises(OSError):
                old_probe.bind(("127.0.0.1", port))
        owner.check_ports_idle([port])
        with tempfile.TemporaryDirectory() as directory:
            child = lab_shell(Path(directory) / "gpu.lock", (port, free_ports()[0]), sleep=False)
            output, error = child.communicate(timeout=3)
            self.assertEqual(child.returncode, 0, error)
            self.assertEqual(output.strip(), "ready")

    def run_control(self, child, command):
        read, write = os.pipe()
        commands = owner.Commands(read)
        try:
            if command is not None:
                os.write(write, json.dumps(command).encode() + b"\n")
            os.close(write)
            write = None
            return owner.supervise(child, commands, "owned", [False], .2)
        finally:
            commands.close()
            os.close(read)
            if write is not None:
                os.close(write)
            if child.poll() is None:
                owner.stop_owned(child, .2)
            if child.stdout:
                child.stdout.close()

    def test_explicit_stop_only_terminates_its_owned_group(self):
        child, foreign = sleeper(), sleeper()
        try:
            result = self.run_control(child, {"op": "stop", "instance_id": "owned"})
            self.assertEqual(result["reason"], "owner_stop")
            self.assertFalse(result["forced_group_stop"])
            self.assertIsNone(foreign.poll())
        finally:
            owner.stop_owned(foreign, .2)
            foreign.stdout.close()

    def test_eof_forces_only_owned_parent_and_descendant(self):
        grandchild = "import signal,time; signal.signal(signal.SIGTERM,signal.SIG_IGN); print('ready',flush=True); time.sleep(60)"
        code = ("import signal,subprocess,sys,time; signal.signal(signal.SIGTERM,signal.SIG_IGN); "
                f"child=subprocess.Popen([sys.executable,'-c',{grandchild!r}],stdout=subprocess.PIPE,text=True); "
                "child.stdout.readline(); print(child.pid,flush=True); time.sleep(60)")
        child = subprocess.Popen([sys.executable, "-c", code], start_new_session=True,
                                 stdout=subprocess.PIPE, text=True)
        descendant = int(child.stdout.readline().strip())
        result = self.run_control(child, None)
        self.assertEqual(result["reason"], "owner_eof")
        self.assertTrue(result["forced_group_stop"])
        # The return itself is the cleanup receipt. No extra grace after it.
        self.assertFalse(live(descendant))

    def test_exited_leader_does_not_hide_live_term_ignoring_descendant(self):
        grandchild = "import signal,time; signal.signal(signal.SIGTERM,signal.SIG_IGN); print('ready',flush=True); time.sleep(60)"
        code = ("import subprocess,sys; "
                f"child=subprocess.Popen([sys.executable,'-c',{grandchild!r}],stdout=subprocess.PIPE,text=True); "
                "child.stdout.readline(); print(child.pid,flush=True)")
        child = subprocess.Popen([sys.executable, "-c", code], start_new_session=True,
                                 stdout=subprocess.PIPE, text=True)
        descendant = int(child.stdout.readline().strip())
        child.wait(timeout=3)
        result = self.run_control(child, None)
        self.assertEqual(result["reason"], "service_exited")
        self.assertTrue(result["forced_group_stop"])
        self.assertFalse(live(descendant))
        self.assertFalse(owner.live_group_members(child.pid))

    def test_unconfirmed_group_cleanup_cannot_return_a_stopped_result(self):
        process = mock.Mock(pid=999999999)
        with mock.patch.object(owner, "live_group_members", return_value=[999999999]), \
                mock.patch.object(owner.os, "killpg"):
            with self.assertRaises(owner.CleanupFailed):
                owner.stop_owned(process, grace_seconds=0, kill_seconds=0)
        process.wait.assert_not_called()

    def test_cleanup_failure_emits_failure_only_and_bootstrap_token_never_enters_argv(self):
        with tempfile.TemporaryDirectory() as directory:
            lease = owner.gpu_lease(Path(directory) / "gpu.lock")
            commands = mock.Mock()
            token = "synthetic_test_credential_0000000000"
            commands.read.return_value = {"protocol_version": 1, "bearer_token": token}
            process = mock.Mock(pid=999999999)
            with mock.patch.object(sys, "argv", ["managed_tts", "--engine", "qwen", "--instance", "0" * 32]), \
                    mock.patch.object(owner, "Commands", return_value=commands), \
                    mock.patch.object(owner.signal, "signal"), \
                    mock.patch.object(owner, "gpu_lease", return_value=lease), \
                    mock.patch.object(owner, "check_ports_idle"), \
                    mock.patch.object(owner.subprocess, "Popen", return_value=process) as launch, \
                    mock.patch.object(owner, "supervise", side_effect=owner.CleanupFailed()), \
                    mock.patch.object(owner, "emit") as emit:
                self.assertEqual(owner.main(), 1)
                self.assertEqual([call.args[0] for call in emit.call_args_list], ["starting", "cleanup_failed"])
                self.assertIs(emit.call_args.kwargs["linux_cleanup_confirmed"], False)
                self.assertIs(emit.call_args.kwargs["process_started"], True)
                self.assertNotIn(token, repr(launch.call_args.args))
                self.assertEqual(launch.call_args.kwargs["env"]["AI_TUTOR_MANAGED_TTS_TOKEN"], token)
                self.assertEqual(launch.call_args.kwargs["env"]["AI_TUTOR_TTS_LEASE_FD"], str(lease))
                self.assertEqual(launch.call_args.kwargs["pass_fds"], (lease,))
                commands.close.assert_called_once()

    def test_preflight_busy_proves_no_child_started_and_leaves_foreign_owner_untouched(self):
        real_lease = owner.gpu_lease
        for reason in ("lease", "port"):
            with self.subTest(reason=reason), tempfile.TemporaryDirectory() as directory, socket.socket() as listener:
                path = Path(directory) / "gpu.lock"
                foreign_lease = real_lease(path) if reason == "lease" else None
                listener.bind(("127.0.0.1", 0))
                listener.listen(1)
                engines = {"qwen": (listener.getsockname()[1], "tutor-qwen-tts", "start_qwen_omni.sh"),
                           "cosy": (free_ports()[0], "tutor-cosy-tts", "start_cosy_omni.sh")}
                commands = mock.Mock()
                commands.read.return_value = {"protocol_version": 1,
                                              "bearer_token": "synthetic_test_credential_0000000000"}
                try:
                    with mock.patch.object(sys, "argv", ["managed_tts", "--engine", "qwen", "--instance", "0" * 32]), \
                            mock.patch.object(owner, "Commands", return_value=commands), \
                            mock.patch.object(owner.signal, "signal"), \
                            mock.patch.object(owner, "ENGINES", engines), \
                            mock.patch.object(owner, "gpu_lease", side_effect=lambda _: real_lease(path)), \
                            mock.patch.object(owner.subprocess, "Popen") as launch, \
                            mock.patch.object(owner, "stop_owned") as stop, \
                            mock.patch.object(owner, "emit") as emit:
                        self.assertEqual(owner.main(), 1)
                        launch.assert_not_called()
                        stop.assert_not_called()
                        self.assertEqual([call.args[0] for call in emit.call_args_list], ["failed"])
                        self.assertIs(emit.call_args.kwargs["process_started"], False)
                        self.assertIs(emit.call_args.kwargs["linux_cleanup_confirmed"], True)
                    listener.settimeout(.05)
                    with self.assertRaises(TimeoutError):
                        listener.accept()  # The foreign listener was never contacted or closed.
                    if foreign_lease is not None:
                        with self.assertRaises(BlockingIOError):
                            real_lease(path)  # Its original owner still holds the lease.
                    else:
                        acquired = real_lease(path)  # Failed port preflight released only our lease.
                        os.close(acquired)
                finally:
                    if foreign_lease is not None:
                        os.close(foreign_lease)

    def test_failure_after_launch_reports_cleanup_only_after_owned_group_is_gone(self):
        with tempfile.TemporaryDirectory() as directory:
            lease = owner.gpu_lease(Path(directory) / "gpu.lock")
            child = sleeper()
            commands = mock.Mock()
            commands.read.return_value = {"protocol_version": 1,
                                          "bearer_token": "synthetic_test_credential_0000000000"}
            try:
                with mock.patch.object(sys, "argv", ["managed_tts", "--engine", "qwen", "--instance", "0" * 32]), \
                        mock.patch.object(owner, "Commands", return_value=commands), \
                        mock.patch.object(owner.signal, "signal"), \
                        mock.patch.object(owner, "gpu_lease", return_value=lease), \
                        mock.patch.object(owner, "check_ports_idle"), \
                        mock.patch.object(owner.subprocess, "Popen", return_value=child), \
                        mock.patch.object(owner, "supervise", side_effect=OSError("synthetic control failure")), \
                        mock.patch.object(owner, "emit") as emit:
                    self.assertEqual(owner.main(), 1)
                    self.assertEqual([call.args[0] for call in emit.call_args_list], ["starting", "failed"])
                    self.assertIs(emit.call_args.kwargs["process_started"], True)
                    self.assertIs(emit.call_args.kwargs["linux_cleanup_confirmed"], True)
                    self.assertIsNotNone(child.poll())
                    self.assertFalse(owner.live_group_members(child.pid))
            finally:
                if child.poll() is None:
                    owner.stop_owned(child, .2)
                child.stdout.close()

    def test_lab_shell_keeps_lease_through_exec_and_blocks_both_owner_types(self):
        with tempfile.TemporaryDirectory() as directory:
            path, ports = Path(directory) / "gpu.lock", free_ports()
            child = lab_shell(path, ports)
            try:
                self.assertEqual(child.stdout.readline().strip(), "ready")
                with self.assertRaises(BlockingIOError):
                    owner.gpu_lease(path)
                competing = lab_shell(path, ports, sleep=False)
                output, _ = competing.communicate(timeout=3)
                self.assertNotEqual(competing.returncode, 0)
                self.assertNotIn("ready", output)
            finally:
                owner.stop_owned(child, .2)
                child.stdout.close()
                child.stderr.close()
            lease = owner.gpu_lease(path)
            os.close(lease)

    def test_managed_lease_is_validated_and_kept_by_real_bash_exec(self):
        with tempfile.TemporaryDirectory() as directory:
            path, ports = Path(directory) / "gpu.lock", free_ports()
            lease = owner.gpu_lease(path)
            child = lab_shell(path, ports, inherited_fd=lease)
            os.close(lease)
            try:
                self.assertEqual(child.stdout.readline().strip(), "ready")
                with self.assertRaises(BlockingIOError):
                    owner.gpu_lease(path)
            finally:
                owner.stop_owned(child, .2)
                child.stdout.close()
                child.stderr.close()

    def test_lab_checks_both_ports_without_contact_or_stopping_the_listener(self):
        with tempfile.TemporaryDirectory() as directory, socket.socket() as listener:
            path = Path(directory) / "gpu.lock"
            listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            listener.bind(("127.0.0.1", 0))
            listener.listen(1)
            for ports in [(listener.getsockname()[1], free_ports()[0]),
                          (free_ports()[0], listener.getsockname()[1])]:
                child = lab_shell(path, ports, sleep=False)
                output, _ = child.communicate(timeout=3)
                self.assertNotEqual(child.returncode, 0)
                self.assertNotIn("ready", output)
                listener.settimeout(.05)
                with self.assertRaises(TimeoutError):
                    listener.accept()
                lease = owner.gpu_lease(path)
                os.close(lease)

    def test_inherited_fd_for_another_file_cannot_bypass_the_common_lock(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "gpu.lock"
            lease = owner.gpu_lease(path)
            other = owner.gpu_lease(Path(directory) / "other.lock")
            try:
                child = lab_shell(path, free_ports(), inherited_fd=other, sleep=False)
                output, _ = child.communicate(timeout=3)
                self.assertNotEqual(child.returncode, 0)
                self.assertNotIn("ready", output)
            finally:
                os.close(other)
                os.close(lease)

    def test_wrong_instance_stops_own_service_as_protocol_failure(self):
        result = self.run_control(sleeper(), {"op": "stop", "instance_id": "other"})
        self.assertEqual(result["reason"], "control_error")

    def test_unexpected_service_exit_is_not_reported_as_owner_stop(self):
        child = subprocess.Popen([sys.executable, "-c", "raise SystemExit(7)"],
                                 start_new_session=True)
        child.wait(timeout=3)
        result = self.run_control(child, None)
        self.assertEqual(result["reason"], "service_exited")
        self.assertEqual(result["service_exit_code"], 7)


class FakeAdmissionControlTests(unittest.TestCase):
    def test_prepared_exact_instance_releases_init_and_exclusive_accepts_noop(self):
        for callback in (None, mock.Mock()):
            process = mock.Mock(returncode=0)
            process.poll.return_value = None
            commands = mock.Mock()
            commands.read.side_effect = [{'op': 'prepared', 'instance_id': 'a'*32},
                                         {'op': 'stop', 'instance_id': 'a'*32}]
            with mock.patch.object(owner, 'stop_owned', return_value=False) as stop:
                result = owner.supervise(process, commands, 'a'*32, [False], on_prepared=callback)
            self.assertEqual(result['reason'], 'owner_stop')
            stop.assert_called_once()
            if callback is not None:
                callback.assert_called_once_with()

    def test_wrong_prepared_instance_is_control_error_without_release(self):
        process = mock.Mock(returncode=0)
        process.poll.return_value = None
        commands, prepared = mock.Mock(), mock.Mock()
        commands.read.return_value = {'op': 'prepared', 'instance_id': 'c'*32}
        with mock.patch.object(owner, 'stop_owned', return_value=False):
            result = owner.supervise(process, commands, 'a'*32, [False], on_prepared=prepared)
        self.assertEqual(result['reason'], 'control_error')
        prepared.assert_not_called()

    def test_pair_wait_stop_or_eof_has_no_child_and_keeps_old_failed_receipt(self):
        for last in ({'op': 'stop', 'instance_id': 'a'*32}, EOFError()):
            commands, admission = mock.Mock(), mock.Mock()
            commands.read.side_effect = [{'protocol_version': 1, 'bearer_token': 'b'*32}, last]
            admission.try_initialization.return_value = False
            with mock.patch.object(sys, 'argv', ['managed_tts', '--root', 'fake-project',
                                                '--engine', 'qwen', '--instance', 'a'*32]), \
                    mock.patch.object(owner.sys, 'stdin', mock.Mock(fileno=lambda: 0)), \
                    mock.patch.object(owner, 'Commands', return_value=commands), \
                    mock.patch.object(owner.signal, 'signal'), \
                    mock.patch.object(owner.gpu_admission, 'load_policy', return_value=owner.gpu_admission.PAIR), \
                    mock.patch.object(owner.gpu_admission.Admission, 'pair', return_value=admission), \
                    mock.patch.object(owner.subprocess, 'Popen') as launch, \
                    mock.patch.object(owner, 'emit') as emit:
                self.assertEqual(owner.main(), 0)
            launch.assert_not_called()
            self.assertEqual([call.args[0] for call in emit.call_args_list], ['waiting_gpu', 'failed'])
            self.assertTrue(emit.call_args.kwargs['linux_cleanup_confirmed'])
            self.assertFalse(emit.call_args.kwargs['process_started'])
            admission.close.assert_called_once()

    def test_pair_rejects_cosy_before_any_child(self):
        commands = mock.Mock()
        commands.read.return_value = {'protocol_version': 1, 'bearer_token': 'b'*32}
        with mock.patch.object(sys, 'argv', ['managed_tts', '--root', 'fake-project',
                                            '--engine', 'cosy', '--instance', 'a'*32]), \
                mock.patch.object(owner.sys, 'stdin', mock.Mock(fileno=lambda: 0)), \
                mock.patch.object(owner, 'Commands', return_value=commands), \
                mock.patch.object(owner.signal, 'signal'), \
                mock.patch.object(owner.gpu_admission, 'load_policy', return_value=owner.gpu_admission.PAIR), \
                mock.patch.object(owner.subprocess, 'Popen') as launch, \
                mock.patch.object(owner, 'emit') as emit:
            self.assertEqual(owner.main(), 1)
        launch.assert_not_called()
        self.assertEqual(emit.call_args.args[0], 'failed')
        self.assertFalse(emit.call_args.kwargs['process_started'])


if __name__ == "__main__":
    unittest.main()
