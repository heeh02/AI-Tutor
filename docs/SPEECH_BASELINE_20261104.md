# 语音与教学模型基线：2026-11-04

本轮运行真实本地模型和真实 Kimi 接口，输入均为自编合成语料。未采集麦克风、未播放扬声器、未使用真实学生档案。工程吞吐、合成样本识别与真人听感分别验收。本文件中的毫秒数据均不能代替麦克风到界面或文字到可听首音的端到端测量。

## 环境与可复现入口

- Windows、本机 RTX 5070 总显存 12227 MiB；本轮 Whisper、Zipformer、Kokoro 均走 CPU。GPU 后续候选尚未测量。
- 隔离环境：`runtime/speech-lab/.venv`，Python 3.10.11；sherpa-onnx/core 2.0.1、numpy 2.2.6、soundfile 0.13.1、cffi 2.1.1、pycparser 3.11。锁定依赖见 `scripts/speech-lab/requirements.txt`，实际环境见 `runtime/goal-baselines/20261103/speech-environment-freeze.txt`。
- 语料：`scripts/speech-lab/education_speech_v1.json`，12 条中文、英文和混合教育表达，涵盖停止、模块作用、否定更正、提示请求、数字与实验比较。
- `speech-corpus` 通过 Windows SAPI 将固定语料合成为 PCM16/16kHz 文件，并可运行现有 Whisper 基线。没有麦克风输入或扬声器输出；文件的声音是合成成人声音。
- `scripts/speech-lab/benchmark.py` 运行真实 Kokoro 合成或按 20ms 音频块进行真实 Zipformer 流式识别。`--pace` 按采样时间送入；结束时立即补 600ms 零样本并 flush，故 `flush_compute_ms` 是松开后计算时间，不能等同自动 VAD 等待结束的时间。
- 所有输出目录必须是新目录，避免覆盖旧失败和测量。`fetch_models.py` 检查大小/哈希、拒绝越界和链接归档项后解包；首次 urllib 下载停滞已停止，curl 重试完成。

## 首轮结果

| 路径 | 测量与结果 | 解释 |
| --- | --- | --- |
| Whisper base，CPU 4线程 | 12条完整转写 7077–7391ms，中位7242.5ms；模型载入270ms | 现有路径主要等待发生在说完后的整句推理 |
| Zipformer bilingual int8，CPU 2线程 | 首次非空 partial 432.8–763.3ms；末音频后的 flush 22.0–25.4ms；模型载入928.8ms | 首 partial 相对开始送音时间，尚不含设备/网络/绘制；不宣称字幕 P95 达标 |
| Kokoro 1.1-zh int8，CPU 2线程 | 首 PCM callback 1239.5–2511.9ms；模型载入1361.6ms；预热后11条首块中位1644.4ms | callback 为分句粒度，未测真实可听首音；尚不满足800ms目标 |
| Kimi stream-json，中文合成请求 | 首stdout374.4ms；首完整assistant8853.5ms；完整验证8894.0ms | 早到版本信息不能作为教学首字 |
| Kimi stream-json，英文合成请求 | 首stdout354.5ms；首完整assistant9721.6ms；完整验证9769.0ms | 两个请求均只有1条完整assistant；不是token级交付 |
| Kimi ACP，合成canary请求 | 98个文字chunk；首chunk6941.2ms，末chunk7050.4ms，prompt完成7074.2ms | 确认增量通路，但首字仍未达到实时目标；与stream-json提示不同，不能直接声称提速百分比 |

原始数据位于 `runtime/goal-baselines/20261103/`：`sapi-corpus/manifest.json`、`kokoro-cpu-first/report.json`、`zipformer-paced-first/report.json`、`kimi-stream-zh-first.log`、`kimi-stream-en-first.log`、`kimi-acp-real-first.log`。ASR 首轮最后样本可能与workspace编译重叠，另跑 `zipformer-paced-clean` 作为无构建重叠复测；不把首次数据改写为独占负载。

只跑了小样本，不把样本分位数解释成产品总体 P95。中文、英文、混合样本分别评估；混合样本的评分不使用声音所属语言过滤字符。

无构建重叠复测 `zipformer-paced-clean/report.json` 已完成：首 partial 431.1–761.9ms，flush 22.1–25.4ms；纯中英和混合识别结果与首轮相同，仍有 `sensor` 和两条混读缺口。复测使用修正后的混合语言评分器。

## 识别与合成缺口

- Zipformer 的10条纯中英样本中，9条在去标点和统一大小写后匹配；英文概念句将 `sensor` 识别成 `CENSOR`，不得用近音错误作为正确术语教学。
- 混合中文声音样本遗漏较多内容；混合英文声音样本漏掉“电机”。SAPI混读发音本身也可能引入问题，须增加独立自然混读语料和另一合成来源，不能只按同一个TTS评判ASR。
- 初版评分器以英文声音语言选择英文分词，漏算了混合句中的中文删除；已修正并增加两项评分回归。首次原始报告保留，正确重评分见 `zipformer-paced-first/rescored.json` 和 `speech-baseline-summary.json`。
- Kokoro提供的中英词典在 `centimeters` 上报告 `OOV word skipped`。这会漏读教育内容，当前不能作为默认声音；后续检查词典/发音回退，并比较Qwen3-TTS、CosyVoice及其他部署设置。
- 自然度、不同年龄真实发音、口音、噪声、声学回声、语音打断仍待验。没有以“生成出了WAV”推断好听或理解正确。

## ACP隔离与取消

精确 Kimi Code 2.3.1 的官方实现支持ACP增量，但其入口不转发现有教学agent参数。新探针使用每轮独立目录中的默认agent覆盖，并读取**同一会话**的实际 `profile.bind`：工具和子代理列表必须为空，系统策略全文必须匹配。静态探针无凭证链接、无prompt；真实探针在前置验证通过后才发合成请求，结束后再次核对有效策略。

首轮静态与真实探针均通过。真实轮未观察到canary内容、工具调用或客户端权限使用；输出不保存回复正文；隔离目录清理和进程回收通过。生产仍保留完整JSON校验后投递，不自动改用未完成的分段合同。

300ms取消探针按设计触发中止，原始日志 `kimi-acp-cancel-first.log` 保留 `success=false` / 退出码1，因为没有完整验证后的回复。取消后的本地进程回收、目录清理与stderr排空成功，cleanup47.3ms；没有记录远端取消确认，不能把它写成完整远程取消通过。

本机实际默认模型为 `kimi-code/k3.1-pro`，元数据标记 `always_thinking`；生产安全配置原本启用thinking、effort为low。首轮观察到thought事件不表示“关闭被忽略”。后续通过ACP实际可选值、设置回执与wire证据确认生效档位；不通过修改元数据强行宣称可关闭推理。

后续 `kimi-acp-thinking-low-first.log` 已完成：session/new声明当前low，可选值为low/high/max；显式设置low的回执与wire有效配置一致。该轮109个文字chunk，首文字8964.1ms，prompt完成10684.4ms，隔离、canary、完整回复校验与清理均通过。证明最低可用档位确实生效，不能声称关闭推理可解决当前等待；网络与模型波动、教学提示和模型选择仍需进一步评估。此轮3项新增协议回归计入最终workspace312通过。

## 来源与许可记录

- [Zipformer模型页](https://huggingface.co/csukuangfj/sherpa-onnx-streaming-zipformer-bilingual-zh-en-2023-02-20)标注Apache-2.0；[官方下载及运行说明](https://k2-fsa.github.io/sherpa/onnx/pretrained_models/online-transducer/zipformer-transducer-models.html)。归档大小511274346字节，本地SHA-256 `27ffbd9ee24ad186d99acc2f6354d7992b27bcab490812510665fa8f9389c5f8`。该旧release没有上游digest，不将本地计算值称为上游校验通过。
- [Kokoro模型页](https://huggingface.co/hexgrad/Kokoro-82M-v1.1-zh)、[官方打包说明](https://k2-fsa.github.io/sherpa/onnx/tts/pretrained_models/kokoro.html)。包内模型LICENSE为Apache-2.0；归档147052523字节，SHA-256 `259c9c8bc906d6078e4e0570421885dabb68b539c2f970fcbb890a58eeaf3e9c` 与GitHub release digest一致。模型许可不替代打包的词典、eSpeak数据和运行库各自的再分发检查。
- [1.1声音映射](https://k2-fsa.github.io/sherpa/onnx/tts/all/Chinese-English/kokoro-multi-lang-v1_1.html)：本轮英文sid0 `af_maple`，中文sid3 `zf_001`，不能沿用1.0的旧编号。
- Kimi精确版本源码和新增诊断说明分别见 `KIMI_INCREMENTAL_BASELINE_20261103.md`、`KIMI_ACP_PROBE_20261104.md`。

当前候选仅用于本地评测，未打包为默认生产模型。下一步优先验证自然语音模型的GPU部署和混读准确度，再接持续转写、句级投递/播报、预热与取消。
