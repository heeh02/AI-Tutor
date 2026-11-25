# Paraformer 与 CosyVoice3 候选核对（2026-11-04）

**后续实测更新（02:40）：**本文保留最初选型依据。Paraformer历史sherpa双语导出与Zipformer的48次实时送音对照已完成，结果与许可差异见 [Paraformer比较](PARAFORMER_COMPARISON_20261104.md)；英文/混读结果没有支持整体替换Zipformer。Cosy已复用冻结WSL栈独立运行，chunk8/128MiB KV下12条暖请求首PCM466–573ms、整卡峰值8137MiB，但冷请求38–71秒和一次56秒块间停顿仍保留；详见 [Cosy本机实测](COSYVOICE3_OMNI_PREPARATION_20261104.md)。以下“未运行/下一轮”描述对应前期研究时间，不是当前状态；自然听感与实际首音仍未验收。

前期仅查官方文档、源码和公开版本元数据，并对照已有项目基线。该前期步骤没有安装依赖、下载模型、启动推理或操作音频设备；未修改 Rust/UI。下文是当时提出的实验合同。现有 Qwen 环境 `vllm-omni 0.32.0 / vllm 0.32.0 / Python 3.12 / cu130` 保持独立。

建议先测 **Paraformer 流式 CPU** 是否修复现有混读/术语缺口。**同日后续核对修正了 CosyVoice3 的环境判断：本机 vllm-omni 0.32.0 已包含正式两阶段 PCM 流式支持，应先复用冻结的 WSL 软件栈，以独立模型目录、缓存和服务配置对照，不需要先安装旧官方 Torch 环境。** 当前 Qwen 服务保持运行；后续 Cosy 实测须由 root 安排 GPU 互斥，不能将两者显存预算简单叠加。最小文件、参考声音与未运行的配置见 [CosyVoice3 Omni 准备记录](COSYVOICE3_OMNI_PREPARATION_20261104.md)。

## 版本与许可证

| 项目 | 本次核对的明确对象 | 许可证边界 |
| --- | --- | --- |
| Paraformer | FunASR PyPI `1.4.21`（2026-11-03 发布）；权重 `funasr/paraformer-zh-streaming`，固定 `apache-2.0-20260804` / `fd2af606b37d7fb8b3b8a218c5be5b07b53ef6ba` | FunASR **代码 MIT**；该权重快照及配套文件 **Apache-2.0**。不要将代码许可套到所有 ModelScope 别名或其他模型。 |
| CosyVoice3 | `FunAudioLLM/Fun-CosyVoice3-0.5B-2512`，权重 SHA `29e01c4e8d000f4bcd70751be16fa94bf3d85a18`；代码 `QwenAudio/CosyVoice` SHA `074ca6dc9e80a2f424f1f74b48bdd7d3fea531cc` | **代码 Apache-2.0**；该模型卡亦标注 **Apache-2.0**。模型目录顶层未见独立 LICENSE，应随所用快照保留模型卡声明；第三方依赖、参考声音分别记录来源与许可。 |

来源：[FunASR 发布包](https://pypi.org/project/funasr/1.4.21/)、[代码 MIT](https://github.com/modelscope/FunASR/blob/main/LICENSE)、[Paraformer 固定快照及许可说明](https://huggingface.co/funasr/paraformer-zh-streaming/tree/apache-2.0-20260804)、[权重 LICENSE](https://huggingface.co/funasr/paraformer-zh-streaming/blob/apache-2.0-20260804/LICENSE)、[CosyVoice 代码 LICENSE](https://github.com/QwenAudio/CosyVoice/blob/074ca6dc9e80a2f424f1f74b48bdd7d3fea531cc/LICENSE)、[CosyVoice 权重快照](https://huggingface.co/FunAudioLLM/Fun-CosyVoice3-0.5B-2512/tree/29e01c4e8d000f4bcd70751be16fa94bf3d85a18)。Paraformer 历史 `45df…` 快照缺 LICENSE，新的许可说明明确不追溯改写旧快照。

## Paraformer：只比较专用流式权重

- 官方模型卡声明 **220M、16kHz、中英、流式**，但定位仍以中文实时识别为主。这不是儿童、长英文或课堂混读准确率的实测证明。`paraformer-en` 离线模型不能代替本候选。先按中文、英文、混合三组分别评分。[模型卡](https://huggingface.co/funasr/paraformer-zh-streaming)、[模型选择说明](https://github.com/modelscope/FunASR/blob/main/docs/model_selection.md)
- Python SDK 已有真正的分块接口：固定模型后调用 `generate(input=chunk, cache=cache, is_final=..., chunk_size=[0,10,5], encoder_chunk_look_back=4, decoder_chunk_look_back=1)`；每次录音独立 cache，末尾必须 flush。示例每块 **9600 样本／600ms**。当前 Rust 的 100ms 传输块可以在适配层合并，不能把 100ms 传输或 200ms UI 轮询写成模型首字幕时延。[SDK 示例](https://github.com/modelscope/FunASR/blob/main/README.md#usage)
- 推荐第一轮维持现有本地 `start/chunk/final/cancel` 合同，只替换独立识别器；显式 `hub="hf"`、模型 revision、`device="cpu"`。先核对输出是块增量还是累计文本，再统一成服务的全句 revision；不要假定每次 `text` 可以直接覆盖字幕。空块、短句、整块结束、取消重开均需协议测试。[流式实现](https://github.com/modelscope/FunASR/blob/v1.4.21/funasr/models/paraformer_streaming/model.py)
- 官方 C++ WebSocket 是另一套 JSON 配置 + PCM 协议；`online` 与 `2pass` 不同，后者增加离线纠正模型。`2pass-online` 示例也出现 `is_final=true`，适配器必须按阶段判断终稿，不可据此自动投递。第一轮不引入第二遍模型及额外显存/延迟变量。[官方通信合同](https://github.com/modelscope/FunASR/blob/main/runtime/docs/websocket_protocol.md)
- Windows 有官方 ONNX/C++ CMake + Visual Studio 路径；WSL/Linux 有 CPU runtime 路径。建议先用独立 Python CPU 环境取得识别质量基线，若依赖不顺转独立 WSL 环境；原生 C++ 打包在选择模型后再做。文档中的 Windows ORT 1.16.1/FFmpeg 构建配方不是本机通过证据。[Windows 构建说明](https://github.com/modelscope/FunASR/blob/main/runtime/onnxruntime/readme.md)

**内存口径：**CPU 路径不占模型 GPU 显存。按 220M 参数粗算，FP32 权重约 0.88GB、FP16 约 0.44GB；只是参数存储算术，不含缓存、算子工作区和其他模块，也不是量化或 GPU 峰值承诺。没有找到此精确快照在本机 RTX 5070 的官方首字幕/显存实测。

## CosyVoice3：真实音频分块优先于包装层“流式”

- 当前推荐权重是 **2512** 版本，覆盖中文、英文等九种语言。默认加载 `llm.pt`，同目录的 `llm.rl.pt` 是另一变体；实验记录必须注明 base/RL，不能拿 RL 的表格成绩描述 base。官方模型卡的中文 CER 1.21%、英文 WER 2.24% 是 **base 的上游自报测试**，不是本项目的教育词汇或真人自然度结论。[模型卡](https://huggingface.co/FunAudioLLM/Fun-CosyVoice3-0.5B-2512)、[加载逻辑](https://github.com/QwenAudio/CosyVoice/blob/074ca6dc9e80a2f424f1f74b48bdd7d3fea531cc/cosyvoice/cli/cosyvoice.py)
- 实际接口是 `AutoModel(...).inference_zero_shot(..., stream=True)` 或 `inference_instruct2(..., stream=True)`，逐次产出 `tts_speech`，采样率取模型属性。准备一份已获授权、来源明确的固定成年参考声音并缓存前缀；第一轮同一声音对照，避免把换音色误算成模型提升。[官方示例](https://github.com/QwenAudio/CosyVoice/blob/074ca6dc9e80a2f424f1f74b48bdd7d3fea531cc/example.py)
- **原生 CLI/FastAPI 路线的三个限制：**官方 FastAPI 虽有 `StreamingResponse` 与 `instruct2` 路由，但调用未传 `stream=True`；流式分支未传数值 `speed`；生成器式文本输入排斥其原生 vLLM 后端。这些结论只针对该源码，不能推论本机 vllm-omni 0.32.0 无流式支持。Omni 已提供完整句子输入、PCM 分块输出，但其 Cosy adapter 仍无原生数值语速，流式非 1.0 语速会被公共接口拒绝。[HTTP 示例](https://github.com/QwenAudio/CosyVoice/blob/074ca6dc9e80a2f424f1f74b48bdd7d3fea531cc/runtime/python/fastapi/server.py)、[模型流式实现](https://github.com/QwenAudio/CosyVoice/blob/074ca6dc9e80a2f424f1f74b48bdd7d3fea531cc/cosyvoice/cli/model.py)、[Omni 0.32.0 TTS](https://github.com/vllm-project/vllm-omni/blob/v0.32.0/docs/user_guide/examples/online_serving/text_to_speech.md#cosyvoice3)
- 官方 **低至150ms** 是上游能力声明，README 未在该处给出可复现实验的硬件、冷暖、输入/输出起止点。不能换算成本机 HTTP 首 PCM、扬声器首音或教学首句时延；论文自然度测试也不能替代本项目双语试听。[官方声明](https://github.com/QwenAudio/CosyVoice/blob/074ca6dc9e80a2f424f1f74b48bdd7d3fea531cc/README.md)、[CosyVoice3 论文](https://arxiv.org/abs/2505.17589)

**环境决策（已修订）：**优先用本机已冻结的 Python 3.12 / cu130 / vllm-omni 0.32.0 软件栈，不安装或混入另一套依赖。独立 Cosy 模型/overlay/cache 与现有 Qwen 分离。只有该路线出现实际不兼容证据，再评估原生 CLI 的另环境配方。原生项目的 Python 3.10、Torch 2.3.1/cu121、平台条件依赖属于另一路线，不能成为否定 Omni 集成的依据。[原生依赖文件](https://github.com/QwenAudio/CosyVoice/blob/074ca6dc9e80a2f424f1f74b48bdd7d3fea531cc/requirements.txt)、[Omni 精确版本说明](https://github.com/vllm-project/vllm-omni/blob/v0.32.0/docs/user_guide/examples/online_serving/text_to_speech.md#cosyvoice3)

RTX 5070 属 Blackwell，不能照搬旧 cu121 配方。PyTorch 2.7 才公布 Blackwell/cu128 支持，下一步需在独立环境解析支持该卡的 Torch + 匹配 torchaudio、验证 ONNX 前端，并冻结实际通过的完整依赖；这不是宣称 Torch 2.7 已验证兼容所有 CosyVoice 组件。WSL 复用 Windows NVIDIA 驱动。[PyTorch 官方说明](https://pytorch.org/blog/pytorch-2-7/)、[NVIDIA WSL 指南](https://docs.nvidia.com/cuda/wsl-user-guide/index.html)

原生 README 的 vLLM 0.9/0.11 配方不是当前 Omni 集成的版本合同。本机 0.32.0 的 pipeline、adapter、deploy 与 S3Tokenizer 已只读核对；首轮建议设置 `COSYVOICE3_TRT=0`，保留两阶段 Torch PCM 路径，避免首次请求额外获取未固定的 FP16 estimator。尚未运行 Cosy 推理，不能把源码和文件准备记为运行通过。[原生 vLLM 用法](https://github.com/QwenAudio/CosyVoice/blob/074ca6dc9e80a2f424f1f74b48bdd7d3fea531cc/README.md#vllm-usage)、[Omni 配置](https://github.com/vllm-project/vllm-omni/blob/v0.32.0/vllm_omni/deploy/cosyvoice3.yaml)

**显存口径：**“0.5B”不是完整推理管线的总参数或显存；还包含 flow、vocoder、tokenizer/ONNX、缓存与工作区。仓库约 9.75GB 是磁盘文件集合，含 base/RL 和不同导出，并非运行显存。源码 vLLM `gpu_memory_utilization=0.2` 也只约束该 LLM 引擎，不能称为整个 TTS 的 20% 上限。没有找到 RTX 5070 12GB 上该固定版本可复现的峰值显存与首 PCM 官方数据。

## 下一轮可执行比较与进入产品的条件

| 顺序 | 实验 | 记录与决定 |
| --- | --- | --- |
| 1 | 独立 Paraformer CPU；复用已有 12 条 WAV 及哈希，按真实时间送入，与 Zipformer/Whisper 同组比较 | 中文 CER、英文 WER、混合 MER；另列 `sensor`、`电机`、数字、否定等关键项。首非空 partial、稳定 partial、final 往返分开记录。若英文/混合仍明显缺词，不因中文成绩好而替换默认。 |
| 2 | Omni 0.32.0 独立 CosyVoice3 base 服务；同 12 条文本、固定项目 SAPI 合成参考声音，预热后 PCM stream；短句和长句各重复 | 加载/参考前缀准备单列；记录请求到模型首 PCM、Rust 收到首 PCM、每块音频时长/间隔、总 RTF、GPU 峰值。参考声影响音色和自然度；不得把预生成 WAV 读取当首包。 |
| 3 | 三档语速、混读术语/单位、长文本、连续轮次、取消后新任务 | 对比 instruction 语速的音频时长和清晰度；不是固定倍率保证。验证取消后无旧音频入队、队列有界；HTTP 断开不等同 GPU 工作已取消。保留 SAPI 回退。 |
| 4 | 候选分别通过后，再与 Qwen + 当前视觉负载联合 | 每进程和整卡显存、ASR 首字幕/TTS 首 PCM/视频帧龄的分布；无 OOM、无持续队列增长。WSL 若缺进程显存查询，写明限制并保留宿主总量及 PyTorch 峰值，不伪造分摊。 |

这些实验复用现有 Rust 录音生命周期与本地服务合同；临时字幕仍不写入草稿，最终结果仍可编辑并由用户确认。选择候选后才把独立适配器接入生产，不新增并行录音所有者。

当前小样本参照：真实 HTTP Zipformer 首 partial **523.5–839.5ms**，final **25.2–43.6ms**；Kokoro CPU 首 callback **1239.5–2511.9ms**。分别见 `docs/ASR_STREAM_SERVICE_20261104.md` 和 `docs/SPEECH_BASELINE_20261104.md`。前者包含送音等待与 HTTP，后者是模型回调；不能直接跨口径排序。12 条成人合成样本只是工程筛选，不能支撑总体 P95、儿童、口音或自然混读结论。

沿用项目的 **700ms 字幕、800ms 首音目标**做后续完整链路验收，先确定起止点再报告是否达标。模型首 PCM 是分解指标；没有浏览器/实际播放测量时必须标“端到端未测”。自然度需要后续获授权的中英试听；短句不漏数字、单位和否定是进入产品前的必要条件。只有测得联合负载稳定，才讨论 GPU 常驻与默认模型切换。
