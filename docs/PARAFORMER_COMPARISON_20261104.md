# Paraformer 历史 ONNX 导出独立对照（2026-11-04）

本实验使用已有 Windows `speech-lab` 环境，文件输入，不连接麦克风、扬声器、ASR 服务或 Rust/UI。默认生产识别器保持 Zipformer。本页将模型身份、识别质量、实时喂入计时分开记录；小样本不能代表儿童、口音、噪声环境或自然课堂混读。

## 模型身份与许可边界

作者新快照 `funasr/paraformer-zh-streaming` / `fd2af606b37d7fb8b3b8a218c5be5b07b53ef6ba`（`apache-2.0-20260804`）只有 PyTorch 权重及配置，没有 ONNX。其明确 Apache-2.0 授权边界见 [候选核对](SPEECH_CANDIDATE_REVIEW_20261104.md)。本轮不为了试跑重新安装 Torch。

选择的是 **sherpa-onnx 官方文档推荐的历史 ONNX 导出**：`csukuangfj/sherpa-onnx-streaming-paraformer-bilingual-zh-en`，固定 commit `8e40c43232a1c5c66c82111efc5820d3accca11b`（2023-08-14）。只下载 `encoder.int8.onnx`、`decoder.int8.onnx`、`tokens.txt`、`README.md`，合计 237,202,916 字节，避免 FP32 和重复归档。目录为 `models/speech/sherpa-onnx-streaming-paraformer-bilingual-zh-en-8e40c43`。

| 文件 | 字节数 | 上游 LFS SHA-256 |
| --- | ---: | --- |
| encoder.int8.onnx | 165462184 | `81a70226a8934e6ed92aa1d4fc486b428b5398e2f2619ed4897b7294cab90e9a` |
| decoder.int8.onnx | 71664561 | `f3cca9f77bb9d93c8fcbfb63ae617b6b1ee96818df3aa3b151c40658fe38594f` |
| tokens.txt | 75756 | 普通 Git 文件；本地 SHA-256 记录于下载清单 |
| README.md | 415 | 普通 Git 文件；本地 SHA-256 记录于下载清单 |

导出发布者模型卡标注 Apache-2.0，说明源模型是 ModelScope `damo/speech_paraformer_asr_nat-zh-cn-16k-common-vocab8404-online`，但没有固定源权重 revision，也未提供可与作者新快照对上的源权重 hash。**这不是已证明与作者新快照相同的模型，也不能用导出模型卡的许可证声明覆盖源权重许可差异。**本轮只作工程评估，生产选用或随产品分发前仍需补完整来源与许可链。新作者快照的 LICENSE 不复制为旧导出的授权凭证。

来源：[sherpa 官方模型说明](https://k2-fsa.github.io/sherpa/onnx/pretrained_models/online-paraformer/paraformer-models.html)、[固定导出模型卡](https://huggingface.co/csukuangfj/sherpa-onnx-streaming-paraformer-bilingual-zh-en/blob/8e40c43232a1c5c66c82111efc5820d3accca11b/README.md)、[转换材料](https://huggingface.co/csukuangfj/streaming-paraformer-zh)、[官方在线文件示例](https://github.com/k2-fsa/sherpa-onnx/blob/master/python-api-examples/online-decode-files.py)。原始 HF API 元数据保存在 `runtime/goal-baselines/20261104/paraformer-source/`，下载文件的实际哈希在模型目录 `download-manifest.json`。

## 固定对照合同

使用 `scripts/speech-lab/paraformer_probe.py`，两个模型均为 `sherpa-onnx 2.0.1 / CPU / 2 线程 / greedy_search / endpoint_detection=false`。Paraformer 调用真实 `OnlineRecognizer.from_paraformer`；Zipformer 调用已有双语 `from_transducer`，encoder/joiner int8、decoder FP32。每条音频新建 stream，不共用缓存；测前用一秒零 PCM 预热算子分配，不给模型语料文本。

两组分别测试、不得合并声称单一准确率：

- SAPI：既有 `runtime/goal-baselines/20261103/sapi-corpus/manifest.json`，12 个固定成年合成 WAV，逐文件核对既有 SHA-256。混读发音本身是混杂因素。
- Qwen：`runtime/goal-baselines/20261104/qwen-chunk8-warm/report.json` 对应 12 个新 WAV，`codec_chunk_frames=8`、模型 revision `85e237c12c027371202489a0ec509ded67b5e4b5`。原文件未改；另存 `qwen-chunk8-asr-input/` 的 16 kHz PCM16 副本。复用 `prepare_tts_roundtrip.py`，`scipy.signal.resample_poly(2,3)` 默认 Kaiser，无裁剪、降噪。既有 tracker-lab Python 3.10.11 / NumPy 2.2.6 / SciPy 1.15.3 做重采样，没有向 speech-lab 新增依赖。新清单记录原 WAV、目标 WAV、源报告哈希。

同一 WAV 由两个模型直接读取，文件和 PCM 哈希进入各自报告。Qwen→ASR 差异包含 TTS、重采样与 ASR，不能归因为 TTS 发音错误，更不是自然度评分。

两种模式分开输出：

1. `nonpaced_quality_only`：20 ms 切块但不等待真实时间，仅比较最终转写。构建/下载争用期间允许运行，报告里的耗时字段不得拿来建立延迟基线。
2. `paced_20ms`：从时刻 0 起，每 20 ms 的真实音频到期才送入；每块解码到不再 ready。记录首非空 partial、首稳定前缀、喂入最大滞后、完整音频结束后的 flush 计算耗时。单条末尾追加 660 ms 人工零 PCM，立即 `input_finished()` 并排空；补零不等待 660 ms，因此 flush 数字不是端点静音等待、HTTP final 往返、UI 字幕或端到端时延。旧 Zipformer 基线用 600 ms，本轮统一使用官方文件示例 660 ms，不能混作完全相同设置。

“首稳定前缀”是事后检查：首次非空 partial 在此后每个更新及 final 中都仍是前缀。它不是模型在线保证稳定性的信号。保留逐次 partial、文字修改次数和 final 是否改写文本，不把小样本最大值称为总体 P95。

评分使用小写英文词、中文单字、混合单字+英文词；排除标点，数字保持字面值，`twenty/20` 与 `二十/20` 不自动视为相同。因此误差率包含书写规范差异，另列 `sensor`、`电机`、否定和数字短语命中。必须结合原文差异解释，不以总平均掩盖混读/否定缺口。

## 运行与验证

仅新增独立脚本与实验输出，没有修改生产 ASR 服务、默认模型、Rust、Cargo 或设备所有权。指标纯测试 3 项通过：混读和数字计分、英文关键项不误用子串匹配、稳定前缀必须跨所有后续更新及最终文本成立。

Windows 工作区根目录示例（Paraformer/Zipformer 的 `--model-kind` 和模型目录分别替换；两组分别运行）：

```powershell
runtime/speech-lab/.venv/Scripts/python.exe scripts/speech-lab/test_paraformer_probe.py
runtime/speech-lab/.venv/Scripts/python.exe scripts/speech-lab/paraformer_probe.py --model-kind paraformer --model models/speech/sherpa-onnx-streaming-paraformer-bilingual-zh-en-8e40c43 --corpus runtime/goal-baselines/20261103/sapi-corpus/manifest.json --corpus-label sapi_adult_12 --output runtime/goal-baselines/20261104/paraformer-compare/nonpaced-sapi-paraformer --load-note "Quality-only run; native build and download may be active."
```

计时模式加 `--pace` 并使用新输出目录，等 native 编译、Qwen 推理基准和模型下载结束后顺序运行，不并行两个模型。报告 `load_note` 必须如实记录观测负载；仍不宣称设备/浏览器链路已验收。

## 本机结果

四组 nonpaced、四组 paced 均完成；每组 12 条。Paraformer 另外启用官方 `is_final=1` 全量复核两组，输出与原始运行逐条一致。所有 paced 最终转写也与各自 nonpaced 完全一致，WAV 哈希一致。输出保存在 `runtime/goal-baselines/20261104/paraformer-compare/`，原始报告没有覆盖。

| 合成音频 | 模型 | 中文字符误差（5 条） | 英文词误差（5 条） | 混合单位误差（2 条） | 术语命中 | 数字/否定短语命中 |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| SAPI | Zipformer | 0/104 | 1/69（1.45%） | 10/25（40%） | 29/34 | 10/10 |
| SAPI | Paraformer 历史导出 | 0/104 | 2/69（2.90%） | 6/25（24%） | 31/34 | 9/10 |
| Qwen codec8 | Zipformer | 4/104（3.85%） | 7/69（10.14%） | 7/25（28%） | 28/34 | 10/10 |
| Qwen codec8 | Paraformer 历史导出 | 0/104 | 16/69（23.19%） | 6/25（24%） | 22/34 | 9/10 |

这些是原文单位的编辑距离，并非听音裁定。例如 Paraformer 的 SAPI `distance sensor` 可完整识别，但英文 `centimeters` 句尾只输出 `cen`；Qwen 样本中 `car` 多次只输出 `c`。Zipformer 的 Qwen `电机` 有一次变为 `点击`，英文 `sensor` 有缺漏。双语混读仍是共同缺口。

句尾专项证据：挑出 5 条截短样本分别补零 660/1320/1980 ms，15 次输出完全相同；再依精确 [sherpa v2.0.1 实现](https://github.com/k2-fsa/sherpa-onnx/blob/v2.0.1/sherpa-onnx/csrc/online-recognizer-paraformer-impl.h) 设置 `is_final=1`，5 次仍完全相同。该设置允许处理最终短特征块，已加进独立探针，随后全 24 条复核无输出变化。证据排除了本组样本简单增加补零就能修复的问题，尚不能把剩余缺词归因于权重、后端尾部积分实现或 TTS 中某一个。报告 `flush-sensitivity.json`、`final-option-sensitivity.json` 保存了各条件。

实时窗口为北京时间 **02:11:19.881–02:15:56.918**；root 暂停编译、下载、推理基准，Qwen 仅常驻空闲，Cosy 未运行。四组顺序运行，不并行推理；桌面背景负载没有被实验性消除。`paced-window.json` 记录 UTC 起止和全部 exit0。

| 合成音频 | 模型 | 首 partial 最小 / 中位 / 最大 | 首稳定前缀最大 | 最终 flush 最小–最大 |
| --- | --- | --- | ---: | --- |
| SAPI | Zipformer | 434 / 761 / 766 ms | 761 ms | 23.5–37.5 ms |
| SAPI | Paraformer 历史导出 | 674 / 686 / 1278 ms | 2487 ms | 55.2–86.8 ms |
| Qwen codec8 | Zipformer | 755 / 1086 / 2048 ms | 2048 ms | 23.2–38.9 ms |
| Qwen codec8 | Paraformer 历史导出 | 1274 / 1280 / 1288 ms | 3684 ms | 54.6–91.6 ms |

喂入最大滞后：Zipformer 各样本约 15.6–16.2 ms，Paraformer 约 24.7–36.6 ms，报告保留调度/解码影响。本轮只从 WAV 起点计时，没有裁掉前导静音或改为人工标注语音起点。Paraformer 精确源码固定的特征块处理意味着 20 ms 传输不能直接变成 20 ms 模型输出间隔。

**结论：保持现有默认。**历史 Paraformer 导出值得作为中文和部分混读候选继续研究，但其英文句尾、Qwen 英文术语和更慢的稳定前缀不足以支持默认替换。下一次应优先验证许可明确的作者快照经官方导出/原生运行时是否复现尾部问题，再考虑产品接线。700 ms 完整字幕、儿童/自然双语准确率和生产来源许可链均未据本轮通过；没有把模型侧 partial/flush 冒充 UI/设备端到端验收。
