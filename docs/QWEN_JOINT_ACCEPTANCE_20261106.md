# Qwen 语音、摄像头与后台记忆联合验收

**后续进展：**待做步骤的读写来源修复已落地，932项回归通过，优化构建的新短联合运行及独立语义审查通过已观察范围，见[修复与复验记录](PENDING_STEP_GROUNDING_20261106.md)。下文保留原始失败，不追认旧run通过。

本轮机械流程通过，**整体语义验收未通过**：第二轮任务状态把教师提出的具体停车方法归给了学习者的原始请求。后台两个记忆候选审查通过，前台持久记忆为空；不能用程序退出成功覆盖任务状态的来源问题。完整 Goal 继续 ACTIVE，实体 PTZ 保持暂缓。

## 实际范围与绑定

在隔离 simulator 中，以同一 Workstation 的生产模块启动 TTS、CPU partial 和 Qwen1.7B final-ASR，使用共享 GPU 策略。两轮均输入同一段合成成人中文 WAV，通过实际收音/VAD逻辑、Qwen终稿、Kimi教学和native CPAL播放；摄像头实际打开，后台两个任务各经历一次抢占及一次声明的恢复。

这次使用 ignored library test 与 debug 摄像头工作进程，**没有真实麦克风、浏览器、声学首音测量、release主程序启动或30分钟运行**。显示ACK及心跳由脚本模拟。只保留相机标量元数据，无目标选择或云台运动。

- 冻结347文件；workspace 914通过、0失败、15忽略。实际模型诊断另行执行。七项离线Python验收检查已在字节相同的v3提案上通过。
- libtest SHA256：`e1f7435351dd4e29a42fb3c0ca1243e09895aa11213985fbcdc806ab257cdd78`。
- debug worker SHA256：`c3b4de279f31505d7cce1ec302435e64a2f48a59987a3095f3cecf3c16059e75`。
- [构建绑定](../runtime/goal-baselines/20261106/qwen-joint-first/binaries/manifest.json)、[实际报告](../runtime/goal-baselines/20261106/qwen-joint-first/actual-first/report.json)、[独立运行审查](../runtime/goal-baselines/20261106/qwen-joint-first/independent-runtime-review.json)。

## 观测结果

| 项目 | 观测与范围 |
| --- | --- |
| 模型准备 | 三路服务约264.96秒就绪；约267.62秒进入相机/交互阶段。 |
| 运行长度 | 原生测试约430.69秒，其中交互观测约160.65秒；外层命令437.66秒。 |
| Qwen终稿 | 两轮均为“先让小车停下来。”，generation 1/3，无CPU或Whisper回退。使用同一WAV，实际捕获样本数不同，不是逐字节相同的PCM实验。 |
| 终稿观察尾部 | 观察到输入关闭→观察到终稿：约973.50/388.21ms；是测试进程观察时间，不是实际麦克风松键到UI显示。 |
| 教学首段 | 提交接受→首次native播放回调：约13992.51/19330.03ms。第二轮另外包含11798.44ms的后台任务协调等待，未并入这两个数值。低延迟目标仍未通过。 |
| 播放 | 共4段Qwen音频，callback/terminal receipt全部完成；`predicted_output_drained=true`，不是声学测量。 |
| 后台任务 | 2任务、4次reservation/starts；各第一次被抢占、同任务第二次恢复，2个shadow候选、0个后台应用记忆。 |
| Provider | 6次请求：2次前台、4次后台；4完成、2取消，0错误/重试。仅证明owned进程启动后的抢占，不证明远端计算已被中断。 |
| 资源 | 86次Windows进程树采样、15次整卡采样，最高约952.3MiB Windows RSS（不含WSL RAM）；整卡峰值10747MiB/12227MiB。30秒GPU采样可能漏掉瞬时峰值。 |
| 相机 | 162次观测、同一session持续有帧；debug worker处理约4fps、预处理约197ms。该数字不能当作release相机性能结论，后续性能验收应使用优化构建。 |
| 清理 | TTS、CPU ASR、Qwen final、camera worker/job、provider和scheduler均确认清理，child exit0，无外层强制终止。用户策略、安装版和已有服务描述文件哈希不变。 |

当前VAD `voice_endpoint_v1` 还包含2500ms结束静音窗口，用来保留思考停顿；本轮最后activity为1940ms，audio endpoint为4440ms。它与终稿推理、教学首段等待是不同阶段，不能用某一阶段的短延迟替代整段体验。

## 独立语义结果

[语义审查](../runtime/goal-baselines/20261106/qwen-joint-first/semantic-review.json)核对了全部候选、逐轮任务状态以及只读不可变SQLite中的实际来源。

- A完整保留“以后讲科学概念，先具体例子、再简短定义”，来源/版本/引句、持久范围均匹配。
- B完整保留一折/两折、每桥一张A4、10cm间距、同样硬币以及尚未实施/无结果，仍是当前活动的待做实验。
- A、B、前台语音属于三个独立学习者，没有互用来源；整个前台持久记忆表为零，不能据此评判未保留的原始前台候选。
- **失败项**：第二轮`pending_step`包含使用设备停止按钮/电源开关、请现场人员协助等教师新增方法，却仅引用原话“先让小车停下来。”，并标记为`model_interpreted_report`。原话支持停止目标，不支持学习者接受了这些方法。该问题不是完成状态幻觉，也不是已执行硬件动作或持久记忆污染；但它确实违反当前证据支持的任务更新要求。

根因是当前快状态写入只验证来源有效且属于当前会话，随后接受自由文本值；合法来源编号不证明值中的所有细节都有依据。历史续接已有更严格的待做计划来源校验，当前活动写入也需要对应约束。原始机械PASS、数据库和失败语义证据均保留，待修复后重新验证，不覆盖本轮结果。

## 下一步

先修复任务状态来源，再用优化构建验证联合性能和30分钟稳定性。启动分析表明ASR加载前半段包含完整模型校验和Python/SDK导入，旧14–21秒的`from_pretrained`计时不等于完整准备；应先拆分计时，再评估同一固定环境的预编译字节码等改进。[启动分析](../runtime/goal-baselines/20261106/qwen-joint-first/startup-review.json)

既有60条Kimi首message全部超过5秒，本地已记录的准备阶段通常约0.57秒，未发现可删的本地数秒固定等待。150ms界面轮询可单独优化，但主要首句突破仍需受控验证provider/model路径，保持原有引用、schema和逐段验证。[教学延迟复核](../runtime/goal-baselines/20261106/qwen-joint-first/teaching-latency-review.json)
