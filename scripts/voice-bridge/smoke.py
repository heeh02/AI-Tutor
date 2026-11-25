"""Exercise the built JSONL helper; real models/playback require explicit flags."""
import argparse
import base64
import json
from pathlib import Path
import queue
import subprocess
import threading
import time


class Bridge:
    def __init__(self, executable, root):
        self.process = subprocess.Popen(
            [str(executable), '--root', str(root)], stdin=subprocess.PIPE,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
            encoding='utf-8', bufsize=1)
        self.events = queue.Queue()
        self.sequence = 0
        self.history = []
        self.started = time.monotonic()
        def read():
            for line in self.process.stdout:
                self.events.put(json.loads(line))
            self.events.put(None)
        threading.Thread(target=read, daemon=True).start()

    def send(self, op, **fields):
        self.sequence += 1
        request = dict(version=1, sequence=self.sequence, id=f'r{self.sequence}', op=op, **fields)
        self.process.stdin.write(json.dumps(request, ensure_ascii=False) + '\n')
        self.process.stdin.flush()
        return request

    def wait(self, event, request=None, timeout=20):
        deadline = time.monotonic() + timeout
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError(f'Waiting for {event}')
            try:
                value = self.events.get(timeout=min(20, remaining))
            except queue.Empty:
                if timeout > 60:
                    self.send('status')
                    continue
                raise
            if value is None:
                raise RuntimeError('Helper exited before expected event')
            self.history.append(dict(observed_seconds=round(time.monotonic() - self.started, 3), **value))
            if value['event'] == 'status' and event != 'status':
                print('Voice phase: ' + value['data']['backend']['phase'], flush=True)
            if value['event'] == event and (request is None or value.get('sequence') == request['sequence']):
                return value
            if value['event'] in ('rejected', 'failed'):
                raise RuntimeError(f"Helper {value['event']}: {value['data'].get('error')}")
            if time.monotonic() >= deadline:
                raise TimeoutError(f'Waiting for {event}')

    def stop(self):
        if self.process.poll() is None:
            self.send('shutdown')
            receipt = self.wait('shutdown', timeout=90)
            assert receipt['data']['cleanup_confirmed'] is True
        # stdin remains open until after exit: checks explicit shutdown behavior.
        code = self.process.wait(timeout=10)
        self.process.stdin.close()
        assert code == 0, self.process.stderr.read()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--executable', type=Path, required=True)
    parser.add_argument('--voice-root', type=Path, required=True)
    parser.add_argument('--report', type=Path, required=True)
    parser.add_argument('--models', action='store_true')
    parser.add_argument('--playback', action='store_true')
    parser.add_argument('--reference', action='store_true', help='Check native render-reference counters; requires an explicitly configured experimental root')
    parser.add_argument('--wav', type=Path, action='append', default=[])
    args = parser.parse_args()
    if args.playback and not args.models:
        parser.error('--playback requires --models')
    if args.reference and not args.playback:
        parser.error('--reference requires --playback')
    if args.report.exists():
        parser.error('report must be a new file')
    bridge = Bridge(args.executable.resolve(strict=True), args.voice_root.resolve(strict=True))
    error = None
    try:
        hello = bridge.wait('hello')
        assert hello['data']['asr']['max_duration_seconds'] == 20
        scope = dict(owner='synthetic-demo', generation=1)
        bridge.wait('bound', bridge.send('bind', scope=scope))
        state = bridge.wait('status', bridge.send('status'))
        assert state['data']['backend']['phase'] == 'idle'
        print('Protocol ready; no models loaded.', flush=True)
        if args.models:
            prepare = bridge.send('prepare')
            bridge.wait('accepted', prepare)
            bridge.wait('completed', prepare, timeout=600)
            print('Owned Qwen pair prepared.', flush=True)
            for wav in args.wav:
                transcript = bridge.send('transcribe', scope=scope,
                    wav_base64=base64.b64encode(wav.read_bytes()).decode('ascii'))
                bridge.wait('accepted', transcript)
                result = bridge.wait('completed', transcript)
                assert result['data']['text'].strip()
                print(f'Transcribed {wav.name}.', flush=True)
            if args.playback:
                for language, text in [('zh', '你好，我们一起学习。'), ('en', 'Hello. Let us learn together.')]:
                    speech = bridge.send('speak', scope=scope, text=text, language=language)
                    bridge.wait('accepted', speech)
                    started = bridge.wait('speech_started', speech)
                    assert started['data']['receipt']['engine_kind'] == 'qwen_pcm'
                    ended = bridge.wait('completed', speech, timeout=60)
                    assert all(item['engine_kind'] == 'qwen_pcm' for item in ended['data']['chunks'])
                    print(f'Played {language} with native receipts.', flush=True)
                speech = bridge.send('speak', scope=scope, language='en',
                    text='Let us compare two measurements carefully. ' * 16)
                bridge.wait('accepted', speech)
                bridge.wait('speech_started', speech)
                cancellation = bridge.send('cancel', operation_id=speech['id'],
                    operation_sequence=speech['sequence'])
                bridge.wait('cancel_requested', cancellation)
                canceled = bridge.wait('canceled', speech)
                assert canceled['data']['stop_error'] is None
                print('Canceled active segmented playback.', flush=True)
                if args.reference:
                    state = bridge.wait('status', bridge.send('status'))
                    reference = state['data']['audio_processing']['render_reference']
                    capture = state['data']['audio_processing']['capture']
                    assert reference['enabled'] is True
                    assert reference['submitted_samples'] > 0 and reference['submitted_blocks'] > 0
                    assert capture['automatic_interruption'] is False
                    print('Observed native reference counters; no microphone or acoustic claim.', flush=True)
    except Exception as exception:
        error = str(exception)
    finally:
        try:
            bridge.stop()
        except Exception as exception:
            error = error or str(exception)
            if bridge.process.poll() is None:
                bridge.process.stdin.close()  # let owned cleanup observe parent EOF
                try:
                    bridge.process.wait(timeout=90)
                except subprocess.TimeoutExpired:
                    bridge.process.terminate()  # only this runner's helper
                    bridge.process.wait(timeout=10)
        args.report.parent.mkdir(parents=True, exist_ok=True)
        with args.report.open('x', encoding='utf-8') as stream:
            json.dump(dict(passed=error is None, error=error, models=args.models,
                playback=args.playback, render_reference_checked=args.reference, microphone_opened=False, acoustic_measurement=False,
                helper_exit=bridge.process.returncode, events=bridge.history),
                stream, ensure_ascii=False, indent=2)
    print(json.dumps(dict(passed=error is None, error=error, report=str(args.report))))
    return int(error is not None)


if __name__ == '__main__':
    raise SystemExit(main())
