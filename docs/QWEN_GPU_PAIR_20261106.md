# RTX 5070：Qwen ASR/TTS 共驻与取消

Qwen3-ASR-1.7B 已接入 Rust 终稿路径，并与 Qwen3-TTS 0.6B 在 12GB RTX 5070 上完成受控共驻及真实工作台生命周期验证。当前为可回退候选，未替换安装版或写入用户启用配置。完整 P0–P5 Goal 继续 ACTIVE。

## 模型质量与交互方式

同一份 ASCEND 语料的 1279 条完整参考上，Qwen 中文 CER/英语 WER/混语 MER 为 **7.09% / 20.63% / 10.08%**，原 Zipformer 为 **11.95% / 50.68% / 24.89%**。全部 1315 条的空终稿从 104 条降至 3 条。数据只有两名成人，不能外推到儿童、噪声或全部教学场景；否定词与数字仍须专项验收。[模型实验](QWEN_ASR_17B_GPU_20261106.md)

实时 partial 继续使用现有流式识别，Qwen 在收音结束后给出更准确的全句终稿。终稿仍可编辑、需确认；加载途中完成的 client 不接管旧录音。全量离线 SDK 后续 1314 次 P50/P95 为 228/542ms，这不是松键到字幕显示时间，也未达到 500ms 尾部 P95 验收。[Rust 接入](QWEN_ASR_RUST_INTEGRATION_20261106.md)

## 显存与共享策略

TTS 保留 CUDA graph，只将 stage0 的 KV cache 固定为 **256MiB**，实际日志确认 2336 tokens 容量。该值仅约束 KV cache，不能当作总显存上限。受控原始服务共驻试验完成 ASR 5 次、TTS 双语预热 2 次及正式合成 5 次；两服务正常清理，整卡采样峰值 **10742MiB**，没有 OOM。[独立复核](../runtime/goal-baselines/20261106/asr-tts-coexist-second/independent-runtime-review.json)

| 受控共驻试验 | 结果与口径 |
| --- | --- |
| ASR 四条交替请求 | 461 / 420 / 354 / 151ms，HTTP 到终稿 |
| ASR/TTS 同时请求 | ASR 短答 261ms；请求区间重叠，不证明 GPU kernel 并行 |
| TTS 五条正式合成 | 首 PCM 26.6–34.2ms，最长分块间隔 100.6–153.0ms |
| 冷/热限制 | TTS HTTP 准备约 249.82 秒，首中文预热首 PCM 2.39 秒；该次已有部分编译缓存 |

首 PCM 不是首个可听词。以上小样本不代表总体 P95、听感或 30 分钟稳定性。首轮共驻在 FlashInfer 编译时因路径空格触发 Ninja 错误，未发送推理请求；失败完整保留。第二轮把编译缓存移到明确的无空格路径后成功，不能将首轮写成显存不足。

默认 GPU 策略仍为 `exclusive`。显式启用共驻需新增以下文件，并为两模型配置各自的 managed policy；TTS 必须使用 Qwen 和 `synthetic_bilingual`，二者必须在同一 WSL distribution/default user 下运行：

```json
{"protocol_version":1,"mode":"qwen_pair_12gb_v1"}
```

文件位置：`runtime/gpu-resource-policy.json`。全局共享锁允许这两个固定角色共驻，角色独占锁阻止重复模型，初始化独占锁使加载/预热依次进行。等待阶段可停止，子进程继承持有期锁；完成预热只释放初始化锁。修改资源模式、发行版或默认 WSL 用户前，应先停止两侧。此策略不是可供任意模型共用显存的通用调度器。

## 正式工作台验证

本次真实模型生命周期测试使用的 release 主程序 SHA256 为 `1c8f931a1fd826d9ae45f48a9c3207457ac12342b701d847720b362079b23fa5`。测试在隔离 fixture 中运行真实 Rust 工作台及两模型，未打开浏览器、麦克风、扬声器或摄像头。[原始报告](../runtime/goal-baselines/20261106/gpu-pair-integration-first/runtime-first/report.json)

1. 观察到 ASR 等待 GPU，Stop 调用约 **31ms** 完成；回执明确 `process_started=false`，这是启动前取消，不是运行中服务的优雅退出。
2. TTS 在启动后约 **126.16 秒**完成双语预热；显式重试 ASR 一次，两侧约 **244.33 秒**全部就绪。不是全新冷缓存基线。
3. 两侧重复 Retry 均返回 400。停用 ASR 约 **2 秒**，TTS 保持同一实例就绪；再停用 TTS 约 **594ms**。
4. 两个已启动服务和工作台正常退出，无强制结束；整卡采样峰值 **10743MiB**。ASR 公共状态不暴露 PID，其清理由 owner 回执核验。

本轮只执行 TTS 双语预热与 ASR 静音预热，没有额外转写/合成或声学输出，不能用就绪状态证明长期并发吞吐。取消旧 TTS 准备任务时，Stop 现在绑定原 nonce 和实例，避免误取或清除新 Retry 的任务；模型清理期间不持有教学控制锁。

## 运行中取消

实际 Rust→HTTP→GPU 测试观察到同一请求 busy 且仍在等待，再发送取消：取消 RPC **2ms**，旧结果不交付；新解码实现到第一次观察空闲约 **152ms**，同实例继续识别 `No.` 为 **140ms**。先前实现对应空闲观察为 2391ms，但缓存状态不配对，不能把全部差异归因于代码。[新实现报告](../runtime/goal-baselines/20261106/final-asr-decode-cancel-first/actual-first/report.json)

另一次直接模型诊断确认生成一个非 EOS token 后命中实际 token 边界取消，随后同模型正常识别短答。该实现可协作停止后续解码，不能中断已经提交的 CUDA kernel、prefill 或编译。原始诊断 `events.jsonl` 的阶段结束 `elapsed_ms` 是阶段耗时；begin/criteria 事件才使用全局起点，不能混成绝对时间线。[token 边界报告](../runtime/goal-baselines/20261106/qwen-asr-decode-cancel-next/decode-boundary-first/report.json)

此前 ASR-only 工作台测试在 loading 阶段 Stop 仍用了约 **15.08 秒**并触发自有组强制结束。新解码取消并不证明加载阶段已经支持快速取消，失败边界保持待优化。

## 验证与交付

首个共享版本的 workspace 回归 **906 通过、0 失败、15 忽略**；Python ASR 32 项、共享 helper 13 项、Linux TTS owner 23 项通过。真实 Linux fcntl/pass_fds 小实验验证双角色、初始化串行及子进程继承租约；它使用私有临时锁，不启动模型。安装器 8 文件 TTS / 6 文件 final-ASR 部署和临时回退检查通过。原生全部 bin 与 WASM release 构建成功，无 warning。

隔离 release 的 130 条合成历史 HTTP 回归通过，另验证未知共享模式及跨发行版配置在 owner 启动前被拒绝。源码/二进制、模型版本、失败试验和回退候选分别归档；README 后续仅文档修订单独标注，不追认成重新编译。[HTTP 回归](../runtime/goal-baselines/20261106/gpu-pair-integration-first/release-smoke-first/report.json)

随后静态审查发现：只比较磁盘策略，不能发现仍在旧发行版驻留的模型；旧 ASR Stop 也可能把刚启动的新 Retry 误报为清理失败。已新增工作台队列锁，绑定实际接受配置中的 distribution，在两侧安全空闲前保持；Stop/finish 不提前释放绑定。TTS 状态的前后 running 读取避免混合快照误判空闲。旧 ASR Stop 使用同锁 nonce/状态快照，若被新操作替代则明确返回 `stop_superseded`，不伪造旧清理回执。[修复静态复核](../runtime/goal-baselines/20261106/gpu-pair-integration-first/pinned-runtime-review.json)

该修复对应 **346 文件**冻结集，新增 8 项回归，完整 workspace **914 通过、0 失败、15 忽略**，无 warning。包含实际 Workstation 队列拒绝路径及并发队列测试，未启动模型。Python 模型实现和 UI 源码未变；前述真实模型结果仍绑定旧的 `1c8f...` 程序，不追认成修复后程序已经重跑模型。外部修改 WSL 默认用户仍不在 distribution 绑定范围内，需先停止服务。

修复后的原生全部 bin 构建和130条合成历史HTTP回归也通过。直接Cargo构建曾因缺少CMake路径失败；使用项目现有`build-rust.ps1`设置MSVC/CMake环境后成功，未修改源码来绕过失败。已归档 [29文件候选](../runtime/goal-releases/20261106-gpu-pair-pinned/manifest.json)，共27,097,036 bytes；主程序SHA256为`af8c1dbe8110ac862d3daa8e2cacbfeb0459055420939a3826beb755b1acfafb`。候选不含模型权重、推理环境或启用policy，未安装、发布；UI及Python资产与上一共享候选逐字节相同。[新版HTTP回归](../runtime/goal-baselines/20261106/gpu-pair-pinning-first/release-smoke-first/report.json)

待验项目包括真实输入噪声/停顿/短答、AEC 双讲、按键到静音、实际首音与自然度、儿童语音、当前模型组合的 30 分钟相机与记忆联合运行、浏览器，以及实际安装回退。教学首句延迟、跨会话教育记忆及视觉跟踪也仍在完整 Goal 内。实体 PTZ 保持用户暂缓安排。

下一步联合诊断的 [v3提案](../runtime/goal-baselines/20261106/qwen-joint-next/proposal-v3/proposal-manifest.json)已独立静态审查通过，7项离线Python验收检查通过；五个拟应用文件仍只在提案目录，未应用、编译Rust或运行模型/设备。它先覆盖两轮语音教学和两个后台记忆任务，保留现有10秒空闲门限，再据实测决定30分钟运行。
