# 本地语音模型来源与离线实测

2026-11-01，Windows 11 / Python 3.10.11 / CPU int8；模型已真实下载并运行。

采用 [Systran/faster-whisper-base](https://huggingface.co/Systran/faster-whisper-base)，是多语言 Whisper base 的 CTranslate2 转换版，包含中文和英文，没有使用只支持英文的 base.en。模型卡声明 MIT，转换自 OpenAI Whisper base。实现采用 faster-whisper 1.2.1，未使用远程 ASR API。

固定版本：`ebe41f70d5b6dfa9166e2c581c45c9c0cfc57b66`（作者仓库 [官方历史](https://huggingface.co/Systran/faster-whisper-base/commits/main)）。

本机直接连接 Hugging Face 超时；实际从 `https://hf-mirror.com` 获取同一固定版本的4个文件，镜像只作为文件传输，不作为新的模型作者。145217532字节 `model.bin` 的 SHA-256 是 `d01c3014881c9c6f3133c182f3d2887eb6ca1c789a7538c5c007196857a0a6a9`，与 [作者原站公布摘要](https://huggingface.co/Systran/faster-whisper-base/blob/main/model.bin) 独立比对一致。小文件依据该版本的 Git blob ID 验证；`models/asr-manifest.json` 记录所有文件 SHA-256、大小、来源、传输地址及验证方式。

`scripts/setup_models.py` 使用连接5秒/读取15秒超时、文件150秒/总360秒时限、每个传输源最多2次尝试；下载到 `.partial`，仅在大小与摘要验证通过后原子替换，失败清理临时文件。默认尝试官方站，失败后使用镜像；可显式选择 `--endpoint official` 或 `--endpoint mirror`。不使用凭证，不运行下载仓库中的代码。

## 已执行的合成语音离线测试

Windows SAPI 本地惠惠和 Zira 分别直接生成临时 WAV，没有通过扬声器播放。测试设置 `HF_HUB_OFFLINE=1`、`TRANSFORMERS_OFFLINE=1`，在 ASR 推理期间禁止 socket 连接；模型以 `local_files_only=True` 从项目目录加载。

| 样本 | 结果 | CPU推理耗时 |
|---|---|---|
| 中文：“电机让小车动起来，测距模块告诉主控离墙有多远。停车距离应该大一点。” | PASS，识别主要内容；“墙”误作“强”，需要人工校正转写 | 859ms |
| 英文：“I want the car to stop before the wall and turn the green light on.” | PASS，完整识别 | 360ms |

证据 `runtime/asr_probe.json`。临时目录在测试后删除，原始测试 WAV 没有保留。

这些是**合成语音样本测试**，不是成人通过真实麦克风进行完整教学对话的验收；真实麦克风、播报、打断与说话声学效果须分别记录。base 模型的中文同音字错误是已知限制，转写默认需确认/修正再发送。

`tests/test_audio.py` 覆盖静音/过短录音不产生确定意图、20秒硬容量、旧代次转写丢弃、仅停止本应用持有的播报进程、半双工和模型缺失诚实报错。
