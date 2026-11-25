# CosyVoice3 / vllm-omni 0.32.0：准备与本机实测

状态（2026-11-04 02:40 更新）：固定文件已下载、校验并实际运行。复用冻结 WSL 软件栈，未安装依赖；先停止 Qwen，再单独运行 Cosy，未播放音频或录音。预热 12 条中英/混读文本全部返回 PCM；冷启动和一次长块间停顿仍失败于实时目标。自然度、实际扬声器首音和联合负载尚未验收。此前“先另建旧官方 Torch 环境”的判断已修正：现有栈已有正式两阶段 PCM 通路。

## 已核对的同版本实现

本机实际软件目录为 `/home/owner/.local/share/ai-tutor-gpu/omni-0.32.0`，已只读核对 native WSL 中的 deploy、adapter、runtime、模型实现和 s3tokenizer。模型实现及 s3tokenizer 的 SHA 与 Windows 挂载旧环境副本相同，后续源码读取没有导入或初始化模型。

| 本机文件 | SHA-256 |
| --- | --- |
| `vllm_omni/deploy/cosyvoice3.yaml` | `9fe73f74e3214bc236abbcc21372c4dbe1a82a8cd2e4e8abd35701dcc3d547a5` |
| `models/cosyvoice3/cosyvoice3.py` | `15b657ebd99056dc6f4afdc4751b44483355247772c5c526001b98738f728096` |
| `models/cosyvoice3/runtime.py` | `85549d327cf130d23944d603b939d95cedf4e884c0cbd9bfde6c9d93bda7e526` |
| `tts_adapters/cosyvoice3.py` | `3e3c3b078271fffe7854dd17b53d0313b48ce057461d0afb4553c9ea083dd41c` |
| `s3tokenizer/__init__.py`（0.3.0） | `35e921400c1733f518922bf8b34440a5769234e850dc8e29ac4f50b668210a8e` |

官方精确 tag 确认：talker → flow/code2wav，输出 24kHz；HTTP `POST /v1/audio/speech` 使用 `stream:true, stream_format:"audio", response_format:"pcm"`。每次需要 `ref_audio` 和准确的 `ref_text`，没有内建音色预设。原生项目的 FastAPI 限制不能用于否定该实现。[0.32.0 官方入口](https://github.com/vllm-project/vllm-omni/blob/v0.32.0/docs/user_guide/examples/online_serving/text_to_speech.md#cosyvoice3)、[同版本客户端](https://github.com/vllm-project/vllm-omni/blob/v0.32.0/examples/online_serving/text_to_speech/cosyvoice3/speech_client.py)

## 固定最小文件及来源

模型 `FunAudioLLM/Fun-CosyVoice3-0.5B-2512`，revision `29e01c4e8d000f4bcd70751be16fa94bf3d85a18`。已保存 11 个上游文件，共 **4,438,936,087 字节**；5 个大文件全部核对上游 LFS SHA-256，6 个小文件核对该 revision 的 Git blob SHA-1，并另存本地 SHA-256。模型卡标 Apache-2.0，保存了对应 README；vllm-omni 和 S3Tokenizer 代码的 Apache-2.0 不替代权重或参考声各自的来源记录。[固定模型快照](https://huggingface.co/FunAudioLLM/Fun-CosyVoice3-0.5B-2512/tree/29e01c4e8d000f4bcd70751be16fa94bf3d85a18)

| 大文件 | 字节数 | 上游 SHA-256 |
| --- | ---: | --- |
| `llm.pt` | 2024669519 | `69f43bd545131c30e98947fb360ea8b4dc9916d8e83dded7757c7ea4f5a24970` |
| `flow.pt` | 1329116148 | `a6fab32a7825e5b0bc855ddd948f8db9370b0a786fbc249caa4595e95b608e4b` |
| `hift.pt` | 83202622 | `b279d7641eb97ae55b3b540cfba4f953c26492a2df758328a89a4d007ab87a65` |
| `campplus.onnx` | 28303423 | `a6ac6a63997761ae2997373e2ee1c47040854b4b759ea41ec48e4e42df0f4d73` |
| `speech_tokenizer_v3.onnx` | 969451503 | `23236a74175dbdda47afc66dbadd5bcb41303c467a57c261cb8539ad9db9208d` |

其余为 BlankEN 的 `config.json/tokenizer_config.json/vocab.json/merges.txt`，顶层原始 `config.json` 和 README。排除 RL 权重、BlankEN 的重复语言模型权重、flow estimator ONNX 和 batch tokenizer；此结论来自 talker 只读 llm.pt、code2wav 读 flow.pt/hift.pt、内部 Qwen 仅从 BlankEN 取配置/词表的代码路径，仍待实际加载验证。[模型加载实现](https://github.com/vllm-project/vllm-omni/blob/v0.32.0/vllm_omni/model_executor/models/cosyvoice3/cosyvoice3.py)、[code2wav 权重加载](https://github.com/vllm-project/vllm-omni/blob/v0.32.0/vllm_omni/model_executor/models/cosyvoice3/cosyvoice3_code2wav.py)

下载前 C 盘余 396.25GB，WSL 根分区仅余 23.93GB（已用 93%），因此模型放 Windows 工作区 `models/speech/Fun-CosyVoice3-0.5B-2512/`，未复制到 WSL 根分区。最终审核时 C 盘余 388.28GB；期间其他任务并行，不把全部空间变化归因于本下载。

- `upstream/`：原文件。顶层 `{}` 原样保留，SHA-256 `44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a`。
- `omni-overlay/`：独立生成 `{"model_type":"cosyvoice3","architectures":["CosyVoice3Model"]}`，其余 9 文件用硬链接，无权重复制。上游空配置不足以选择 Omni 模型类。[上游空配置](https://huggingface.co/FunAudioLLM/Fun-CosyVoice3-0.5B-2512/blob/29e01c4e8d000f4bcd70751be16fa94bf3d85a18/config.json)
- `runtime/speech-cosy/cache/s3tokenizer/speech_tokenizer_v3_25hz.onnx`：同一已校验 tokenizer 的硬链接。s3tokenizer 默认从 ModelScope 取该内容并转为 Torch 上 GPU，查找这个独立缓存名；不是直接读取模型目录中的 ONNX。缓存须在未来 Cosy 进程设置 `XDG_CACHE_HOME` 指向本独立目录。[S3Tokenizer 下载与固定摘要](https://github.com/xingchensong/S3Tokenizer/blob/main/s3tokenizer/__init__.py)
- `download-manifest.json`：每文件实际来源、校验口径、overlay/hash、缓存方式。最终审计确认 9 个 overlay 硬链接和 tokenizer 缓存确实指向同一文件；没有仅凭大小视为校验成功。

权重来自发布者的 ModelScope 副本，字节摘要与固定 HF revision 相同。ModelScope 的 README 不同、顶层 config 缺失，首次尝试失败；直接 curl 到 HF 超时后，用本机 PowerShell HTTPS 路径取得精确 HF 文件并核对 Git blob。`download-first.log` 及错误副本保留，第二轮成功见 `download-second.log`。独立脚本 `scripts/speech-lab/prepare_cosyvoice3.py` 已加入这个小文件传输路径，没有改 Qwen 下载器。

## 12GB 单并发候选（已运行两轮）

上游默认为两阶段各 8 并发、GPU 比例 0.4/0.2、codec chunk 15、左上下文 25；注释明确针对 H100 调参，不是 RTX5070 的实测。H200 的 packed/MPS 配置不适用于本卡。[精确默认 deploy](https://github.com/vllm-project/vllm-omni/blob/v0.32.0/vllm_omni/deploy/cosyvoice3.yaml)

首轮保持两阶段与 RAS，各 1 并发、max_model_len/max_num_batched_tokens 4096、预算 0.30/0.20、两阶段 eager、chunk15/left25。实测 stage0 仍预留 2.39GiB KV、209056 tokens，远多于单并发需要；整卡抽样峰值 10456MiB。原配置另存为 `cosyvoice3-chunk15-first-tested.yaml`。当前 `scripts/speech-lab/cosyvoice3_12gb_candidate.yaml` 改为 chunk8，并显式限制 stage0 KV 为 134217728 字节；启动报告 10912 tokens，足够单个4096上下文，暖请求整卡峰值 8137MiB。这两项同时改变，不能把全部下降归因于分块。S3Tokenizer、CUDA 工作区、HiFT FP32 和桌面占用不被两个 GPU 比例完整约束。

未来独立进程建议：`COSYVOICE3_TRT=0`、`COSYVOICE3_BATCH_FLOW=0`、`COSYVOICE3_PACKED_STREAMING=0`、`COSYVOICE3_FULL_RESPONSE_OPTIMIZATIONS=0`、`COSYVOICE3_REFERENCE_WORKERS=1`，配独立 XDG/HF 缓存及离线模式。TRT 默认会尝试获取额外的 `yuekai/...FP16-ONNX`；本轮未下载此模型，显式关闭 TRT 避免首次请求产生未审核下载/编译。[对应配置与额外来源](https://github.com/vllm-project/vllm-omni/blob/v0.32.0/vllm_omni/transformers_utils/configs/cosyvoice3.py)

Qwen Rust 探针完成后已停止 18868 服务。Cosy 首次服务于02:18:57就绪，第二配置服务于02:28:02就绪（127.0.0.1:18869）；启动脚本 `scripts/speech-lab/start_cosy_omni.sh` 使用独立缓存、离线模式及上述环境开关。没有改变冻结依赖、全局驱动或生产默认配置；两种 GPU 语音模型未同时运行。

## 已准备的合成参考声与验收

复用项目自写 SAPI 成人合成语料，不要求用户录音。`reference-audio.json` 核验了中文 `zh_concept.wav`（6.265s/16kHz）与英文 `en_concept.wav`（6.475s/16kHz）的原始哈希和逐字文本。API 接收 WAV data URL，adapter 内部转换到 24kHz 并缓存参考条件；不要使用官方客户端默认远程参考音频而遗漏来源记录。参考声音与发音会影响生成音色和自然度，SAPI 参考不是自然人或儿童语音质量证据。[adapter 处理](https://github.com/vllm-project/vllm-omni/blob/v0.32.0/vllm_omni/entrypoints/openai/tts_adapters/cosyvoice3.py)

首次实例化、首次参考条件准备、暖请求分别记录。复用 12 条中/英/混读教育文本，先测 HTTP 首 PCM、音频时长/块间隔、预缓冲下界与总显存，再经 Rust 队列验证取消/抢占/连续播放。原生数值语速未实现：流式 `speed!=1` 会被公共接口拒绝，不能静默忽略 UI 三档语速。[适配器能力默认值](https://github.com/vllm-project/vllm-omni/blob/v0.32.0/vllm_omni/entrypoints/openai/tts_adapters/base.py)

准备阶段的完整证据目录为 `runtime/goal-baselines/20261104/cosyvoice3-preparation/`，最终 `final-audit.json` 与模型 manifest 一起核对。下面补充随后实际运行的记录；没有把模型运行成功记作自然度提升或 800ms 实际首音通过。

准备工具的 Python 语法检查通过；新增小文件 HTTPS 路径已用固定 config 重新下载并核验，记录 `transport-probe.json`。候选 YAML 已用现有 PyYAML safe_load 解析并检查两阶段单并发，没有导入模型或运行服务；这不代替 vLLM 引擎配置/模型加载验证。

## 实际 PCM 与延迟记录

以下目录均位于 `runtime/goal-baselines/20261104/`；完整报告含逐块到达时刻、实际配置快照/摘要、参考音频摘要及负载说明。使用 `benchmark_qwen_http.py --engine cosy`，没有打开音频设备。

| 报告目录 | 状态/样本 | 首 PCM | 结果与限制 |
| --- | --- | --- | --- |
| `cosy-pcm-cold-first` | chunk15，首次请求2条 | 中文71102ms、随后英文750ms | 首次调用约72秒；同时有原生定向测试编译，只作冷启动功能记录 |
| `cosy-pcm-warm-first` | chunk15，12条 | 570–706ms | 全部完成；中文概念句总计58654ms、最大块间隔56616ms，保留为明显失败，原因未证实 |
| `cosy-pcm-warm-second` | chunk15，同12条再次运行 | 562–680ms | 总计1068–2852ms，未重复58秒停顿；整卡峰值10456MiB |
| `cosy-chunk8-cold-first` | 重启、chunk8/128MiB KV，2条 | 38777ms、3358ms | 持久编译缓存保留，原生构建并行；仍不能冷启动实时 |
| `cosy-chunk8-warm-first` | chunk8/128MiB KV，12条 | **466–573ms** | 总计1156–3020ms、音频2–6.88秒；整卡峰值**8137MiB**，无其他推理/构建基准，仍含桌面背景占用 |

当前配置的 `buffer-analysis.json` 按真实网络块到达时间计算理想播放时钟：240ms音频在466–573ms可用。大多数样本不需额外等待，`mixed_zh` 在此预缓冲后仍存在约172ms缺口，理想无断裂起播至少726ms。WAV能量启音估计为120–220ms，因此请求到启音的理想下界多数约598–749ms，混读该条约926ms。能量阈值不是听觉评价；这些下界不含实际声卡/扬声器，也不能支撑总体P95或800ms通过。首轮冷慢、长停顿均保留，未从总体记录剔除。

## 离线转写对照与决策

对 chunk15 第二轮12条完整WAV按固定2/3重采样为16k，另存 `cosy-asr-input/corpus.json`，不改原音频。相同2线程CPU识别器非实时回放，仅用于内容检查：

| 识别器/报告 | 中文编辑错误 | 英文编辑错误 | 混读编辑错误 | 教育术语 | 关键数字/否定等 |
| --- | --- | --- | --- | --- | --- |
| Zipformer / `cosy-asr-zipformer` | 2/104 | 1/69 | 4/25 | 30/34 | 10/10 |
| Paraformer / `cosy-asr-paraformer` | 1/104 | 5/69 | 4/25 | 29/34 | 10/10 |

转写误差同时受TTS和ASR影响，不能单独归咎生成模型；成人SAPI参考影响音色，也不是真人或儿童自然听感证据。当前保持Qwen与Cosy双候选，生产默认未切换。真实扬声器、按键到静音、长期并发和人工双语试听分别待验。

## Rust实际传输检查（02:53）

客户端的中英独立参考声及SHA绑定、Cosy/Qwen请求字段和真实PCM回执规则已纳入517项完整回归及release构建。`tts-stream-probe --engine cosy`通过运行中的18869服务：中文首包630.7ms、83520样本/3.48秒完整音频、总1782ms；英文502.0ms、74880样本/3.12秒、总1584ms。独立900ms取消探针首包542.7ms、已收到23040样本，取消约15.6ms后返回，旧样本消费为0。日志分别为 `tts-rust-cosy-zh-first.log`、`tts-rust-cosy-en-first.log`、`tts-rust-cosy-cancel-first.log`。

这些探针只消费有界PCM队列，不打开设备，不产生实际播放收据，也未收到远端取消ACK；未创建生产descriptor。首条是服务空闲后新文本，不能直接混入前述紧接预热12条的分布。后续后台预热管理是下一片，不属于这里已经通过的客户端范围。
