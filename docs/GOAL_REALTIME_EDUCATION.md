# 实时中英教育助手升级 Goal

状态：ACTIVE。用户于 2026-11-03 授权按已审核方案推进。主程序保持 Rust；模型可运行在隔离的本地推理服务中；覆盖较大年龄跨度和中英双语。无预设 token 预算。

最新检查点见 [2026-11-04实施进展](GOAL_PROGRESS_20261104.md)；真实语音/模型结果见 [基线报告](SPEECH_BASELINE_20261104.md)。

用户于2026-11-04补充实施要求：尽量模块化，方便维护升级；不做无意义的防御性编程，专注核心功能。按视频传输、语音推理、记忆整理等实际职责划分模块；必要的归属/时效/资源约束服务于正确性，不扩展为与本项目无关的通用框架。

## 完成定义

语音、视觉、教学记忆、工作台和资源调度共同工作；每阶段留下源码、可复现测量、失败记录和回退方案。需要真人听感、儿童自然语音、实体运动的验收单独记录，未实际执行不能记作通过，也不能据此宣布整个 Goal 完成。先前用户暂缓实体云台测试的状态继续有效。

| 阶段 | 必须交付 | 验收方式 | 当前状态 |
| --- | --- | --- | --- |
| P0 基线与选型 | 端到端时序、冷/热资源基线、中英教育语料；Kimi 官方流式/ACP 能力验证；Zipformer、Paraformer、Qwen3-TTS 0.6B、CosyVoice3 0.5B、Kokoro 的本机兼容、许可与样本比较 | 真实模型测量；合成和自然语音分开；记录版本、模型来源、哈希和限制 | 进行中 |
| P1 实时语音 | 连续转写、可编辑终稿、检查后发送和可选自动轮换；自然中英声音、分句播放、常驻预热、有界缓冲、按键停止、代次隔离、轻量回退；参考信号回声消除和语音打断 | 转写质量、首音、尾部、取消、断连、切会话；回声和自然打断单独实测 | 连续转写/PCM/分句队列已接线；预热管理与AEC继续实施，设备待验 |
| P2 实时视觉 | 分阶段耗时、独立预览、二进制最新帧推送、同帧选择；所选目标歧义、时间戳/跳帧/预测；ByteTrack 与 OC-SORT 同源比较 | 多人/遮挡/交叠/非线性运动回放；并发帧龄和丢帧；保持身份/新鲜度边界 | 基础改动中 |
| P3 教育记忆 | 有来源事件、快状态、可取消恢复且前台优先的 LLM 整理、版本化慢记忆和关联；中英混合召回、重要性与可信度分离、幂等与预算 | 跨会话对照；来源/矛盾/提示程度；档案隔离、纠正删除及派生失效；先影子评测再逐类启用 | 基础改动中 |
| P4 教学与工作台 | 独立界面/教学语言、阅读和提示深度、语速；分层教学、实验对比、记忆解释/修订、历史分页与滚动；统一调度、故障恢复、安装回退 | 中英、不同能力、长对话、并发、资源限制、30 分钟联合运行 | 基础改动中 |
| P5 实体云台 | 保留历史失败、标定和保护；有限位置控制、死区/平滑、实际运动延迟补偿 | 用户可参与时检验跟随、遮挡、丢失、急停、拔插和持续运行 | 软件可推进；现场测试暂缓 |

## 性能目标与口径

参考目标：字幕 P95 ≤700ms；松开后转写尾部 P95 ≤500ms；预热 TTS 文本到实际首音 P95 ≤800ms；按键到静音 P95 ≤150ms；720p 24–30fps、应用采集到呈现 P95 争取 ≤200ms；模型接口达标时，说完到有效首句 P50 ≤2.5s/P95 ≤5s。基线后说明可实现门槛和未达原因，不通过改变口径伪造通过。

必须分别报告模型初始化、首次调用、预热后、独占与联合负载、中英与混合语言。推理回调产生首块 PCM 不等于扬声器发出首音；camera.frame 返回后的时间不等于传感器曝光时间；离线快速回放不等于实时麦克风表现。P95 样本数不足时列原始值，不包装成可靠总体结论。

硬门槛：测试集跨档案泄漏、删除记忆复活、计划误记完成、错误切换所选目标均为 0。GPU 依据本机 RTX 5070 的实测 12227 MiB 总显存规划，同时保留系统/显示与运行波动空间。

## 什么值得长期记住

- 明确学习目标、已实际完成的尝试与结果，以及来源和时间。
- 概念理解的可观察证据、使用过的提示程度；不能因助手讲过就推定学生掌握。
- 可修订的误解、有效提示策略；一次失误不固化成能力标签。
- 明确表达的偏好，区分本次临时要求与长期偏好；语言、语速、难度分别管理。
- 用户接受的待做实验和待验证假设，状态必须区别于完成。
- 更正、遗忘要求和替代版本，失效传播到摘要、关联、检索缓存和排队整理任务。

重要性决定是否保留/召回，可信度取决于证据类型，两者分开。重复摘要、翻译与模型推断不是独立证据。后台任务绑定 namespace、learner、session、epoch、事件范围、证据版本和提示版本，前台交互优先。历史硬件配置重新确认，不靠记忆启动设备。

架构参考：Generative Agents（UIST 2023）的反思与检索、MemoryBank（AAAI 2024）的更新与遗忘、A-Mem（NeurIPS 2025）的记忆关联。采用适合本项目的机制，不照搬论文系统，也不从通用记忆基准推断教育有效性。LoCoMo（ACL 2024）补充长会话评测思路，核心仍是教育场景证据集。

## 推进与证据

每次集成先备份源码；真实用户数据库不用于测试。原已安装版本保持可回退，完成对应验证后才发布新阶段。

- 既有发布基线：workspace 261 通过、0 失败、1 项真实付费探针忽略；见 `OPTIMIZATION_REPORT_20261103.md`。
- 本 Goal 源码备份：`runtime/goal-backups/20261103/`。
- 本 Goal 基线产物：`runtime/goal-baselines/20261103/`；语音测试环境：`runtime/speech-lab/.venv`，不更改系统 Python。
- 当前并行工作：完整有效记忆检索/分页与教育语料、独立摄像头预览和历史滚动、所选目标歧义和阶段时序、语音环境及真实模型基线。
- 后续检查点：每个阶段追加已通过/失败/待验及证据路径；不能用“代码完成”替代集成或现场验收。

## 一手资料

- [Generative Agents](https://doi.org/10.1145/3586183.3606763)
- [MemoryBank](https://ojs.aaai.org/index.php/AAAI/article/view/29946)
- [A-Mem](https://proceedings.neurips.cc/paper_files/paper/2025/hash/19909c36f51abc4856b4560aff3d36d6-Abstract-Conference.html)
- [LoCoMo](https://aclanthology.org/2024.acl-long.747/)
- [sherpa-onnx Rust API](https://k2-fsa.github.io/sherpa/onnx/rust-api/index.html)
- [Zipformer 流式模型](https://k2-fsa.github.io/sherpa/onnx/pretrained_models/online-transducer/zipformer-transducer-models.html)
- [Kokoro 中英模型](https://k2-fsa.github.io/sherpa/onnx/tts/pretrained_models/kokoro.html)
- [Qwen3-TTS 0.6B](https://huggingface.co/Qwen/Qwen3-TTS-12Hz-0.6B-CustomVoice)
- [Qwen3-TTS 推理配置](https://github.com/vllm-project/vllm-omni/blob/main/recipes/Qwen/Qwen3-TTS.md)
- [CosyVoice](https://github.com/QwenAudio/CosyVoice)
- [Paraformer streaming](https://huggingface.co/funasr/paraformer-zh-streaming)
- [ByteTrack](https://www.ecva.net/papers/eccv_2022/papers_ECCV/papers/136820001.pdf)
- [OC-SORT](https://openaccess.thecvf.com/content/CVPR2023/html/Cao_Observation-Centric_SORT_Rethinking_SORT_for_Robust_Multi-Object_Tracking_CVPR_2023_paper.html)
