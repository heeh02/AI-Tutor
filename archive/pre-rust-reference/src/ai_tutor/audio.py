"""Local push-to-talk and Windows installed speech. No cloud audio APIs."""
from __future__ import annotations

import asyncio
import base64
import math
import os
import subprocess
import threading
import time
from pathlib import Path

import numpy as np
import sounddevice as sd
from scipy.signal import resample_poly


class AudioService:
    def __init__(self, root: Path):
        self.root = root
        self.model_path = root / "models" / "asr" / "faster-whisper-base"
        self.stream = None
        self.buffer = None
        self.sample_count = 0
        self.sample_rate = 48000
        self.record_generation = 0
        self.device_id = None
        self.lock = threading.RLock()
        self.model = None
        self.model_lock = threading.Lock()
        self.asr_inflight = False
        self.speech = None
        self.speech_generation = 0
        self.speech_lock = asyncio.Lock()
        self.state = "idle"
        self.error = None
        self.last_asr_ms = None
        self.last_rms = None
        self.last_speech_ms = None
        self.voices = []

    def devices(self):
        devices = sd.query_devices()
        return [{"device_id": i, "name": d["name"], "input_channels": d["max_input_channels"],
                 "sample_rate": d["default_samplerate"], "hostapi": sd.query_hostapis(d["hostapi"])["name"]}
                for i, d in enumerate(devices) if d["max_input_channels"] > 0]

    def status(self):
        return {"state": self.state, "error": self.error, "asr_provider": "faster-whisper / CPU int8 / base multilingual",
                "asr_ready": (self.model_path / "model.bin").exists(), "asr_local": True,
                "tts_provider": "Windows System.Speech", "tts_local": True, "voices": self.voices,
                "device_id": self.device_id, "recording": self.stream is not None,
                "asr_ms": self.last_asr_ms, "speech_ms": self.last_speech_ms, "rms": self.last_rms,
                "max_record_seconds": 20, "raw_audio_saved": False}

    async def probe(self):
        script = "Add-Type -AssemblyName System.Speech; $s=New-Object System.Speech.Synthesis.SpeechSynthesizer; @($s.GetInstalledVoices() | ForEach-Object { $_.VoiceInfo.Name }) | ConvertTo-Json -Compress; $s.Dispose()"
        process = await asyncio.create_subprocess_exec("powershell.exe", "-NoProfile", "-NonInteractive", "-Command", script, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE, creationflags=subprocess.CREATE_NO_WINDOW)
        try:
            out, _ = await asyncio.wait_for(process.communicate(), 12)
            import json
            self.voices = json.loads(out.decode("utf-8", errors="replace").strip().lstrip("\ufeff"))
            if isinstance(self.voices, str):
                self.voices = [self.voices]
        except Exception:
            if process.returncode is None:
                process.kill()
                await process.wait()
            self.error = "无法探测已安装的本地声音"
        return self.status()

    def start_recording(self, device_id=None):
        with self.lock:
            if self.stream is not None or self.asr_inflight or self.state == "transcribing":
                raise ValueError("请先结束当前录音或转写")
            devices = self.devices()
            if not devices:
                raise ValueError("未发现本机麦克风")
            if device_id is None:
                preferred = [d for d in devices if "OBSBOT" in d["name"].upper() and "WASAPI" in d["hostapi"]]
                preferred = preferred or [d for d in devices if "OBSBOT" in d["name"].upper()]
                device_id = preferred[0]["device_id"] if preferred else devices[0]["device_id"]
            info = sd.query_devices(device_id, "input")
            self.sample_rate = int(info["default_samplerate"])
            self.buffer = np.zeros(self.sample_rate * 20, dtype=np.float32)
            self.sample_count = 0
            self.record_generation += 1
            self.device_id = device_id
            self.error = None

            def callback(indata, frames, timing, status):
                count = min(frames, len(self.buffer) - self.sample_count) if self.buffer is not None else 0
                if count:
                    self.buffer[self.sample_count:self.sample_count+count] = indata[:count, 0]
                    self.sample_count += count
                if self.buffer is None or self.sample_count >= len(self.buffer):
                    raise sd.CallbackStop

            self.state = "recording"
            stream = None
            try:
                stream = sd.InputStream(device=device_id, samplerate=self.sample_rate, channels=1, dtype="float32", callback=callback)
                stream.start()
                self.stream = stream
            except Exception:
                if stream is not None:
                    stream.close()
                if self.buffer is not None:
                    self.buffer.fill(0)
                self.buffer = None
                self.sample_count = 0
                self.state = "idle"
                raise
            return {"device_id": device_id, "sample_rate": self.sample_rate, "generation": self.record_generation}

    def discard_recording(self):
        with self.lock:
            self.record_generation += 1
            if self.stream is not None:
                stream, self.stream = self.stream, None
                try:
                    stream.abort()
                except Exception:
                    self.error = "麦克风连接已中断，请重新选择输入设备"
                finally:
                    try:
                        stream.close()
                    except Exception:
                        self.error = "麦克风连接已中断，请重新选择输入设备"
            if self.buffer is not None:
                self.buffer.fill(0)
            self.buffer = None
            self.sample_count = 0
            self.state = "transcribing" if self.asr_inflight else "idle"

    def _transcribe(self, samples, rate, language):
        if not (self.model_path / "model.bin").exists():
            raise ValueError("本地语音模型尚未下载，请运行 scripts/setup_models.py")
        with self.model_lock:
            if self.model is None:
                from faster_whisper import WhisperModel
                self.model = WhisperModel(str(self.model_path), device="cpu", compute_type="int8", cpu_threads=4, local_files_only=True)
            divisor = math.gcd(rate, 16000)
            samples = resample_poly(samples, 16000 // divisor, rate // divisor).astype(np.float32)
            segments, info = self.model.transcribe(samples, language="zh" if language.startswith("zh") else "en", beam_size=3,
                initial_prompt="主控、电机、测距、超声波、停车距离、绿灯。control, motor, distance sensor, light.",
                vad_filter=True, condition_on_previous_text=False, no_speech_threshold=0.6)
            result = [s for s in segments if s.no_speech_prob < 0.6 and s.avg_logprob > -1.2]
            return "".join(s.text for s in result).strip()

    async def stop_recording(self, language="zh"):
        with self.lock:
            if self.stream is None:
                raise ValueError("没有进行中的录音")
            stream, self.stream = self.stream, None
            failed = False
            try:
                stream.stop()
            except Exception:
                failed = True
            finally:
                try:
                    stream.close()
                except Exception:
                    failed = True
            if failed:
                if self.buffer is not None:
                    self.buffer.fill(0)
                self.buffer = None
                self.sample_count = 0
                self.state = "idle"
                self.error = "麦克风连接已中断，请重新选择输入设备"
                raise ValueError(self.error)
            samples = self.buffer[:self.sample_count].copy()
            self.buffer.fill(0)
            self.buffer = None
            rate, generation = self.sample_rate, self.record_generation
            self.asr_inflight = True
            self.state = "transcribing"
        start = time.monotonic()
        worker = None
        def release_asr(job=None):
            samples.fill(0)
            self.last_asr_ms = round((time.monotonic() - start) * 1000)
            with self.lock:
                self.asr_inflight = False
                if self.state == "transcribing":
                    self.state = "idle"
            if job is not None and not job.cancelled():
                job.exception()  # Drain an abandoned local worker's error.
        try:
            self.last_rms = float(np.sqrt(np.mean(samples ** 2))) if len(samples) else 0
            if len(samples) < rate * 0.45 or self.last_rms < 0.002:
                return {"text": "", "error": "录音太短或声音过轻，请重录", "rms": self.last_rms}
            worker = asyncio.create_task(asyncio.to_thread(self._transcribe, samples, rate, language))
            text = await asyncio.shield(worker)
            if generation != self.record_generation:
                return {"text": "", "error": "录音结果已取消"}
            return {"text": text, "rms": self.last_rms, "requires_confirmation": True,
                    "error": None if text else "未获得可靠转写，请重录或输入文字"}
        finally:
            if worker is not None and not worker.done():
                worker.add_done_callback(release_asr)
            else:
                release_asr(worker)

    async def _terminate_speech(self, process):
        if process is not None and process.returncode is None:
            try:
                process.kill()
                await asyncio.wait_for(process.wait(), 5)
            except ProcessLookupError:
                pass
            except asyncio.TimeoutError:
                raise ValueError("本地语音进程未能及时停止")

    async def stop_speech(self):
        # Invalidate pending process creation before awaiting its startup lock.
        self.speech_generation += 1
        generation = self.speech_generation
        async with self.speech_lock:
            process, self.speech = self.speech, None
            await self._terminate_speech(process)
            if generation == self.speech_generation and self.state == "speaking":
                self.state = "idle"

    async def speak(self, text, language="zh", expected_generation=None):
        voice = "Microsoft Huihui Desktop" if language.startswith("zh") else "Microsoft Zira Desktop"
        # Script is constant; text is decoded solely as stdin data and never evaluated.
        script = "$ErrorActionPreference='Stop'; Add-Type -AssemblyName System.Speech; $s=New-Object System.Speech.Synthesis.SpeechSynthesizer; $p=([Console]::In.ReadToEnd() | ConvertFrom-Json); $s.SelectVoice($p.voice); $s.SetOutputToDefaultAudioDevice(); try { $s.Speak($p.text) } finally { $s.Dispose() }"
        import json
        start = time.monotonic()
        async with self.speech_lock:
            if expected_generation is not None and expected_generation != self.speech_generation:
                return False
            if self.state in {"recording", "transcribing"} or self.asr_inflight:
                return False
            if voice not in self.voices:
                raise ValueError("指定语言的本地声音尚未安装")
            self.speech_generation += 1
            generation = self.speech_generation
            previous, self.speech = self.speech, None
            await self._terminate_speech(previous)
            if generation != self.speech_generation:
                return False
            if self.state == "speaking":
                self.state = "idle"
            process = await asyncio.create_subprocess_exec("powershell.exe", "-NoProfile", "-NonInteractive", "-EncodedCommand", base64.b64encode(script.encode("utf-16le")).decode(), stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.DEVNULL, stderr=asyncio.subprocess.PIPE, creationflags=subprocess.CREATE_NO_WINDOW)
            if generation != self.speech_generation or self.state in {"recording", "transcribing"}:
                await self._terminate_speech(process)
                return False
            self.speech = process
            self.state = "speaking"
        try:
            _, err = await asyncio.wait_for(process.communicate(json.dumps({"text": text, "voice": voice}, ensure_ascii=True).encode("ascii")), 90)
            if generation != self.speech_generation:
                return False
            if process.returncode:
                raise ValueError("本地语音播报失败")
            return True
        except asyncio.TimeoutError:
            await self.stop_speech()
            raise ValueError("本地播报超时")
        finally:
            self.last_speech_ms = round((time.monotonic() - start) * 1000)
            if generation == self.speech_generation:
                self.speech = None
                self.state = "idle"

    async def close(self):
        self.discard_recording()
        await self.stop_speech()
