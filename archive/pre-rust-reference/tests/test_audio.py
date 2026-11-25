import asyncio
from pathlib import Path

import numpy as np
import pytest

from ai_tutor.audio import AudioService


class FakeStream:
    def __init__(self, **kwargs):
        self.callback = kwargs.get("callback")
        self.closed = self.started = self.aborted = False

    def start(self):
        self.started = True

    def stop(self):
        pass

    def close(self):
        self.closed = True

    def abort(self):
        self.aborted = True


@pytest.mark.parametrize("duration,amplitude", [(2.0, 0), (.1, .3)])
def test_silence_or_too_short_recording_does_not_invent_input(tmp_path, duration, amplitude):
    service = AudioService(tmp_path)
    stream = FakeStream()
    service.stream = stream
    service.sample_rate = 16000
    service.buffer = np.full(int(duration*16000), amplitude, dtype=np.float32)
    service.sample_count = len(service.buffer)
    def forbid_inference(*args):
        raise AssertionError("ASR must not run for silence/too-short input")
    service._transcribe = forbid_inference
    result = asyncio.run(service.stop_recording())
    assert result["text"] == ""
    assert result["error"]
    assert service.buffer is None and service.stream is None
    assert stream.closed and service.state == "idle"


def test_recording_buffer_has_hard_twenty_second_bound(tmp_path, monkeypatch):
    from ai_tutor import audio
    service = AudioService(tmp_path)
    monkeypatch.setattr(service, "devices", lambda: [{"device_id": 7, "name": "OBSBOT Mic", "hostapi": "Windows WASAPI"}])
    monkeypatch.setattr(audio.sd, "query_devices", lambda *args: {"default_samplerate": 16000})
    monkeypatch.setattr(audio.sd, "InputStream", FakeStream)
    result = service.start_recording()
    assert result["device_id"] == 7
    assert len(service.buffer) == 320000
    stream = service.stream
    data = np.ones((320100,1), dtype=np.float32)
    with pytest.raises(audio.sd.CallbackStop):
        stream.callback(data, len(data), None, None)
    assert service.sample_count == 320000
    service.discard_recording()
    assert stream.closed and stream.aborted
    assert service.buffer is None


def test_cancelled_asr_does_not_deliver_late_transcript(tmp_path):
    service = AudioService(tmp_path)
    service.stream = FakeStream()
    service.sample_rate = 16000
    service.buffer = np.ones(16000, dtype=np.float32) * .1
    service.sample_count = 16000
    def late_transcript(*args):
        service.record_generation += 1
        return "stale words"
    service._transcribe = late_transcript
    result = asyncio.run(service.stop_recording())
    assert result["text"] == ""
    assert result["error"]


def test_speech_stop_kills_only_owned_process(tmp_path):
    class Child:
        returncode = None
        killed = False
        waited = False
        def kill(self):
            self.killed = True
            self.returncode = -9
        async def wait(self):
            self.waited = True
    service = AudioService(tmp_path)
    owned = Child()
    service.speech = owned
    service.state = "speaking"
    generation = service.speech_generation
    asyncio.run(service.stop_speech())
    assert owned.killed and owned.waited
    assert service.speech is None and service.state == "idle"
    assert service.speech_generation == generation + 1


@pytest.mark.parametrize("state", ["recording", "transcribing"])
def test_half_duplex_refuses_playback_while_listening(tmp_path, state):
    service = AudioService(tmp_path)
    service.state = state
    assert asyncio.run(service.speak("Hello")) is False


def test_missing_model_reports_unavailable_instead_of_fake_transcript(tmp_path):
    service = AudioService(tmp_path)
    with pytest.raises(ValueError, match="scripts/setup_models.py"):
        service._transcribe(np.ones(16000, dtype=np.float32),16000,"zh")


def test_stop_during_speech_process_creation_kills_child_without_sending_text(tmp_path, monkeypatch):
    from ai_tutor import audio
    async def scenario():
        service = AudioService(tmp_path)
        service.voices = ["Microsoft Huihui Desktop"]
        creating, release = asyncio.Event(), asyncio.Event()
        class Child:
            returncode = None
            killed = False
            communicated = False
            def kill(self):
                self.killed = True
                self.returncode = -9
            async def wait(self):
                return self.returncode
            async def communicate(self, data):
                self.communicated = True
                raise AssertionError("Cancelled speech must not receive text")
        child = Child()
        async def delayed_create(*args, **kwargs):
            creating.set()
            await release.wait()
            return child
        monkeypatch.setattr(audio.asyncio, "create_subprocess_exec", delayed_create)
        speech = asyncio.create_task(service.speak("old learner words", expected_generation=0))
        await creating.wait()
        stop = asyncio.create_task(service.stop_speech())
        await asyncio.sleep(0)
        release.set()
        assert await speech is False
        await stop
        assert child.killed and not child.communicated
        assert service.speech is None and service.state == "idle"
    asyncio.run(scenario())


def test_stale_queued_speech_token_does_not_create_a_process(tmp_path, monkeypatch):
    from ai_tutor import audio
    async def scenario():
        service = AudioService(tmp_path)
        service.voices = ["Microsoft Huihui Desktop"]
        token = service.speech_generation
        await service.stop_speech()
        async def forbidden(*args, **kwargs):
            raise AssertionError("Stale playback must never spawn")
        monkeypatch.setattr(audio.asyncio, "create_subprocess_exec", forbidden)
        assert await service.speak("stale learner", expected_generation=token) is False
    asyncio.run(scenario())


def test_queued_playbacks_share_one_creation_owner(tmp_path, monkeypatch):
    from ai_tutor import audio
    async def scenario():
        service = AudioService(tmp_path)
        service.voices = ["Microsoft Huihui Desktop"]
        creating, release, completed = asyncio.Event(), asyncio.Event(), asyncio.Event()
        calls = []
        class Child:
            returncode = None
            def kill(self):
                self.returncode = -9
                completed.set()
            async def wait(self):
                return self.returncode
            async def communicate(self, data):
                await completed.wait()
                return b"", b""
        async def delayed_create(*args, **kwargs):
            calls.append(1)
            creating.set()
            await release.wait()
            return Child()
        monkeypatch.setattr(audio.asyncio, "create_subprocess_exec", delayed_create)
        first = asyncio.create_task(service.speak("first", expected_generation=0))
        await creating.wait()
        second = asyncio.create_task(service.speak("second", expected_generation=0))
        release.set()
        assert await second is False
        await service.stop_speech()
        assert await first is False
        assert len(calls) == 1
    asyncio.run(scenario())


def test_failed_input_start_releases_stream_and_buffer(tmp_path, monkeypatch):
    from ai_tutor import audio
    service = AudioService(tmp_path)
    monkeypatch.setattr(service, "devices", lambda: [{"device_id": 7, "name": "Mic", "hostapi": "WASAPI"}])
    monkeypatch.setattr(audio.sd, "query_devices", lambda *args: {"default_samplerate": 16000})
    stream = FakeStream()
    def fail_start():
        raise OSError("device busy")
    stream.start = fail_start
    monkeypatch.setattr(audio.sd, "InputStream", lambda **kwargs: stream)
    with pytest.raises(OSError, match="device busy"):
        service.start_recording()
    assert stream.closed
    assert service.buffer is None and service.stream is None and service.state == "idle"


def test_cancelled_asr_keeps_single_worker_bound_until_inference_finishes(tmp_path):
    import threading
    async def scenario():
        service = AudioService(tmp_path)
        service.stream = FakeStream()
        service.sample_rate = 16000
        service.buffer = np.ones(16000, dtype=np.float32) * .1
        service.sample_count = 16000
        entered, release = threading.Event(), threading.Event()
        captured = []
        def delayed_transcript(samples, *args):
            captured.append(samples)
            entered.set()
            assert release.wait(3)
            return "old transcript"
        service._transcribe = delayed_transcript
        transcription = asyncio.create_task(service.stop_recording())
        assert await asyncio.to_thread(entered.wait, 2)
        service.discard_recording()
        transcription.cancel()
        with pytest.raises(asyncio.CancelledError):
            await transcription
        assert service.asr_inflight and service.state == "transcribing"
        assert np.any(captured[0])
        with pytest.raises(ValueError):
            service.start_recording()
        release.set()
        async def drained():
            while service.asr_inflight:
                await asyncio.sleep(.01)
        await asyncio.wait_for(drained(), 2)
        assert not np.any(captured[0])
        assert service.state == "idle"
    asyncio.run(scenario())


@pytest.mark.parametrize("discard", [True, False])
def test_disconnected_input_still_releases_resources(tmp_path, discard):
    class DisconnectedStream(FakeStream):
        def stop(self):
            raise OSError("USB disconnected")
        def abort(self):
            raise OSError("USB disconnected")
    service = AudioService(tmp_path)
    stream = DisconnectedStream()
    samples = np.ones(16000, dtype=np.float32)
    service.stream = stream
    service.buffer = samples
    service.sample_count = len(samples)
    if discard:
        service.discard_recording()
    else:
        with pytest.raises(ValueError):
            asyncio.run(service.stop_recording())
    assert stream.closed and service.stream is None and service.buffer is None
    assert not np.any(samples)
    assert service.error and service.state == "idle"
