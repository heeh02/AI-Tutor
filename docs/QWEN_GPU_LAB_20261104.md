# Qwen3-TTS 本机 GPU 服务验证

状态：进行中，尚未接入生产语音或打开扬声器。所有请求仅使用项目自写教育语料；首 PCM 与实际首音分开记录。

## 固定输入

Qwen/Qwen3-TTS-12Hz-0.6B-CustomVoice，HF revision `85e237c12c027371202489a0ec509ded67b5e4b5`。13 个模型文件的大小和 SHA 见模型目录 `download-manifest.json`，完整复核见 `runtime/goal-baselines/20261104/qwen-model-final-hash-audit.log`。主权重经 Qwen 的 ModelScope 发布源获取，SHA 与固定 HF revision 一致；原失败下载记录保留。

硬件为 RTX 5070 12GB。使用隔离的 WSL Ubuntu 环境，Python 3.12.14、PyTorch 2.13.0+cu130、vLLM/vLLM-Omni 0.32.0。部署配置基于同版本官方 recipe，单并发、小模型上下文、两阶段流式解码。原始上游配置另存于基线目录。

## 失败与修复

1. `qwen-omni-start-first.log`：当前 CLI 不接受 `--disable-log-requests`。移除该参数；本地安装源码中 `enable_log_requests` 默认 false，未打开请求正文日志。
2. `qwen-omni-start-second.log`：环境位于 Windows 挂载目录，约 74,569 个文件；模型阶段初始化超过 300 秒，未就绪。没有通过放宽超时把该次记成成功。
3. 将精确版本依赖安装到 `/home/owner/.local/share/ai-tutor-gpu/omni-0.32.0`；原环境保留。安装时修改正在读取的脚本导致结尾解析失败，包安装已完成；随后独立 GPU 核验通过才写环境路径指针。脚本已恢复完整且通过语法检查。
4. `qwen-omni-start-third.log`：原生目录启动明显更快，约三分钟进入采样内核编译；遇到明确的 CUDA 编译器/头文件不匹配错误。依赖解析得到 NVCC/CRT/NVVM 13.4.92，而 PyTorch 所需 runtime headers 为 13.0。核验 CUDA 张量计算不能覆盖该 JIT 编译路径。
5. 将隔离环境的三个编译组件固定为 13.0.88，其余运行库保持原版本；`gpu-compiler-pin.log` 保留安装变更。`verify_gpu_env.py` 同时核验编译器与 runtime 主次版本和真实 CUDA 计算；`requirements-omni-linux.txt` 固定这组版本。没有改全局 CUDA 驱动或系统 Python。
6. `qwen-omni-start-fourth.log`：CUDA 源码已成功编译，但链接器找不到 `-lcudart/-lcuda`。wheel 将 runtime 放在 `lib/` 且仅有带版本名文件，FlashInfer 使用传统 `lib64/` 搜索路径。新增仅作用于该服务的 `cuda_link_env.sh`，在隔离环境建立两个链接并设置进程环境；独立 ninja 链接检查通过。
7. `qwen-omni-start-fifth.log`：两阶段成功就绪，01:34:44 完成服务启动。此前失败记录均保留。

版本选择依据：[NVIDIA NVCC 13.0.88 发布](https://pypi.org/project/nvidia-cuda-nvcc/13.0.88/)、[CUDA 13.0.1 组件版本](https://docs.nvidia.com/cuda/archive/13.0.1/pdf/CUDA_Toolkit_Release_Notes.pdf)、[vLLM-Omni GPU 安装说明](https://docs.vllm.ai/projects/vllm-omni/en/latest/getting_started/installation/gpu/)。头文件不匹配也已由本地 FlashInfer 所带 CCCL 的兼容检查直接证实，未关闭该检查。

## 可复现入口

`setup_gpu_linux_env.sh` 安装精确依赖并复核 GPU；`start_qwen_omni.sh` 读取环境路径、在 loopback 18868 启动，凭证保存在本地文件，不写日志。`benchmark_qwen_http.py` 接收 PCM，记录首次/后续样例、首块时间、块间隔、结束时间、WAV 和抽样显存；不操作音频设备。

只有服务实际就绪且产生完整音频后才记录推理通过。自然度、双语混读准确性、扬声器延迟、真人语音交互与联合负载仍需各自证据。

## 第一批实际音频

`qwen-pcm-second/report.json` 的首次中文短句首 PCM 为 2915ms，总生成4049ms；随后的英文短句首 PCM 为46.8ms。这是首次调用开销，不能混入预热测量或省略。

预热后 `qwen-pcm-full-first/report.json` 共12句（中5、英5、混读2）全部收到 HTTP200 和完整、非空、偶数字节 PCM。首字节26.1–79.9ms，总生成375–1155ms，音频长度2.0–8.08秒；抽样总显存峰值7973MiB，含桌面进程。首次短句批次的抽样峰值为7981MiB。样本数很小，未宣称总体P95通过，也没有测扬声器出声。

输出位于对应目录的WAV。该阶段只证明服务产生音频：尚未完成听感、发音准确性、有效声信号起点、播放缓冲下溢或真人验收。首块短、后续块间隔较大，Rust播放端仍需实测预缓冲；不能把网络首字节直接当连续可听首音。

当前vLLM-Omni的语速校验会拒绝不支持原生速度控制模型的非1.0流式请求（本地 serving_speech.py 的流式请求校验）。后续接线不得悄悄忽略工作台语速，须实现有效速度处理或明确保留兼容播报路径。

## 分块改进与质量边界

保留原25帧配置 `qwen3-tts-chunk25-tested.yaml` 后，将后续codec分块改为8帧（首块仍1帧、左侧上下文仍72帧、不裁剪静音）。第六次服务启动成功，日志 `qwen-omni-start-sixth.log`；cold批首个中文请求首PCM3285ms，随后英文30ms。新配置及SHA随新报告保存。

`qwen-chunk8-warm/report.json` 的12条预热样例全部完整输出，首PCM27.6–50.4ms。`buffer-analysis.json` 基于真实接收时序计算：240ms音频可用约135–157ms，理想连续播放启动下界55–77ms；25帧批次相应约314–375ms与234–295ms。此对照不是严控统计实验，采样输出时长会变化，但较小分块确实让第二批音频更早可用。实际Rust队列和设备仍需独立测试。

三段连续20ms RMS≥0.005的启发式声信号起点仍约420–680ms；不等同音素开始或人耳听见。未通过裁剪静音或改写起止点声称800ms目标已达标。

另将12条完整WAV以scipy多相滤波24k→16k另存，再通过现有Zipformer离线识别，结果见 `qwen-asr-roundtrip/report.json`。存在“电机→点击”、遗漏sensor/开头词、混读音译等错误；数字与否定的若干样例保留。该结果混合TTS、重采样、ASR误差，不能单独归因于TTS，也不能用它作为自然度评分。原始音频未修改、未打开麦克风或扬声器；中英混读和发音质量仍需独立识别器与试听核验。

## Rust 传输验收（02:10）

新的 release `tts-stream-probe` 通过正式 Rust `TtsClient`、有界 channel 和 20ms PCM 包接入当前 chunk8 服务，未打开音频设备。中文/英文合成句分别完整收到 92160/88320 个样本（3.84/3.68 秒）；首包约 301/38ms，总传输约 911/606ms。服务长期空闲后第一条耗时高于此前紧接预热批，不能把它删掉或只报告较快后续值。日志 `tts-rust-zh-first.log` / `tts-rust-en-first.log`。

100ms 后推进取消代次的独立探针接到首块后被取消，取消到 Rust 返回约15.6ms；没有消费过期样本，没有生产配置文件或远端取消 ACK 证明，见 `tts-rust-cancel-first.log`。这些结果验证传输与取消，不证明 CPAL 出声、用户按键到静音或自然度。
