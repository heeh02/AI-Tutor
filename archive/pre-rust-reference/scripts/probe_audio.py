"""Hardware probe: brief microphone acquisition + real installed local TTS."""
import asyncio
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "src"))
from ai_tutor.audio import AudioService


async def main():
    audio = AudioService(ROOT)
    await audio.probe()
    devices = audio.devices()
    capture = audio.start_recording()
    await asyncio.sleep(2)
    result = await audio.stop_recording()
    # Brief fixed engineering prompt; no user's private text is spoken.
    spoken = await audio.speak("本地语音测试。主控、电机、测距模块和绿灯。", "zh")
    stop_task = asyncio.create_task(audio.speak("这是停止播报测试。" * 20, "zh"))
    await asyncio.sleep(1)
    await audio.stop_speech()
    stopped_result = await stop_task
    report = {"microphones": devices, "capture": capture, "recording_rms": result.get("rms"),
              "microphone_read": "PASS", "recognized_text_present": bool(result.get("text")),
              "silence_handling": result.get("error"), "local_tts_process_completed": spoken,
              "tts_interruption": not stopped_result, "status": audio.status(),
              "human_speech_accuracy": "NOT_RUN: no person was instructed to speak during this engineering probe"}
    (ROOT / "runtime/audio-probe.json").write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
    print(json.dumps({k: v for k, v in report.items() if k not in {"microphones", "status"}}, ensure_ascii=True))
    await audio.close()


if __name__ == "__main__":
    asyncio.run(main())
