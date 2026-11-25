# 待做步骤来源修复与优化构建复验

已修复“来源编号有效，却把老师新增的方法记成学习者待办”的问题。新写入校验、旧记录的读取投影、界面及实际短联合运行均已有证据；此前失败不改判。完整P0–P5 Goal继续ACTIVE。

## 行为变化

`pending_step`自动写入必须由每个有效、同学习者/当前会话来源支持。它可以复制明确声明的`pending_experiment`、已接受计划的完整文本，或学习者当前完整原话；只忽略外围空白，不改问号、否定、数字、单位和大小写。新增方法或释义不匹配就拒绝这一候选，保留原待办、目标概括、原始对话和正常教学回复。明确清空必须来自实际存在且为空串的结构化声明，缺失字段不算授权。

旧数据通过共享校验重新投影。未经支持的待办及其来源标签/field evidence不进入模型的当前任务与压缩任务引用，界面显示“待核对”，原文在关闭的未核实记录详情中保留。`Store::session()`只添加顶层派生标记，**不修改原始state或证据，也不在读取时保存状态**，避免其他写路径克隆状态后意外覆盖原记录。历史续接/结束整理的原有规则保持不变。

该约束仅针对待做步骤；一般目标概括仍可合理释义，不能据此宣称所有自由文本的语义归因已经自动得到保证。[实现](../rust/ai-tutor/src/pending_step.rs)、[上下文投影](../rust/ai-tutor/src/context.rs)、[界面](../rust/tutor-ui/src/activity_summary.rs)

## 软件与候选

- 新写入修复：349文件冻结，923项回归通过。
- 完整读写修复：350文件冻结，**932通过、0失败、15忽略**，其中UI133项；7项Python验收检查通过。原生、WebAssembly及优化版ignored诊断构建均成功，无构建warning。
- 新release的130条合成历史HTTP回归通过，并确认旧待办被标记需核对、原数据库state未变、空默认不显示核对提醒。浏览器视觉验收仍未执行。
- 已归档[29文件候选](../runtime/goal-releases/20261106-task-grounding/manifest.json)，27,110,527 bytes；主程序SHA256 `e30ee3ebdc4fa16d8e7b392cd404c5166ac85ea3c67aaa397ae30c4ec9639123`。源码和提示词已更新，候选未安装或发布，用户启用policy未改。

[构建绑定](../runtime/goal-baselines/20261106/task-grounding-read-first/optimized-binaries/manifest.json)、[HTTP回归](../runtime/goal-baselines/20261106/task-grounding-read-first/release-smoke-first/report.json)

## 优化构建的实际联合运行

使用release优化的library test和camera worker，复用生产Workstation模块。两轮仍为同一成人合成中文WAV，Qwen final实际识别，Kimi实际回复，Qwen/CPAL实际提交音频；相机真实采集，后台两任务各抢占一次并恢复一次。没有真实麦克风、浏览器、声学首音或30分钟运行，也不是release主程序listener的完整语音验收。

| 项目 | 实测 |
| --- | --- |
| 三路语音准备 | 221.91秒；缓存条件未配对，不把与旧run的差值当作修复收益。 |
| 流程长度 | 外层398.57秒，原生392.62秒，交互观测167.43秒。 |
| Qwen final | 两次均“先让小车停下来。”，generation 1/3，无CPU/Whisper回退。 |
| 终稿观察尾部 | 观察输入关闭→观察终稿约460.42/419.78ms。仅两条，不是实际松键/UI总体P95。 |
| 首次native播放回调 | 提交接受后约20438.72/11419.26ms；第二轮额外13303.14ms的后台协调等待另列。实际声学首音未测。 |
| TTS | 共6段native CPAL完成回执，预测输出已排空；未做声音自然度评分。 |
| 720p相机 | 169次同session观测，处理fps中位25.75，处理37.42ms、预处理7.34ms中位，采样帧龄最大118ms。未测浏览器呈现或真实遮挡跟踪。 |
| 资源 | 78次Windows树/13次GPU采样，RSS峰值967,004,160 bytes（不含WSL RAM），整卡峰值10722MiB/12227MiB。粗采样不保证捕获瞬时峰值。 |
| 后台与退出 | 2任务、4次starts/reservations、2 shadow候选、0后台应用记忆；全部owned服务/进程正常清理，零自动重试。 |

[实际报告](../runtime/goal-baselines/20261106/task-grounding-read-first/joint-optimized-first/report.json)、[独立运行复核](../runtime/goal-baselines/20261106/task-grounding-read-first/independent-runtime-review.json)

独立语义审查通过本次已观察范围：A偏好、B完整未实施计划的内容/来源/版本/引句保持，三学习者不串用，前台持久记忆仍为零，两轮都没有存入未经接受的具体方法。原始前台候选未保存，不能据此判断究竟省略还是拒绝了某个候选。本次新fixture未触发旧记录屏蔽分支；该分支的证据来自Store/context/UI测试和release HTTP回归。[语义审查](../runtime/goal-baselines/20261106/task-grounding-read-first/semantic-review.json)

## ASR启动拆分

另用诊断副本计时，生产engine/协议未改：一次加载、一次静音预热，零正式转写。成功v4进程正常退出，监听线程确认join，独占租约及自有进程组清理完成。

| 阶段 | 成功v4 host wall time |
| --- | ---: |
| 完整模型校验 | 12.05秒 |
| SDK校验 | 0.04秒 |
| 导入及CUDA准备 | 35.99秒 |
| `from_pretrained` | 14.34秒 |
| 加载未归属开销（含末尾同步） | 0.005秒 |
| 静音预热 | 4.85秒 |

总load62.42秒，worker67.39秒，外层69.29秒；707个浮点参数张量均为CUDA0/BF16，整卡采样峰值6245MiB。首次v2计时也完成了load/warm，但退出为signal6，整体失败：导入/CUDA69.78秒、校验18.04秒、权重15.19秒。两次缓存未配对，**没有安装预编译字节码、移动环境或证明启动优化收益**。[成功结果](../runtime/goal-baselines/20261106/asr-startup-next/actual-v4-first/report.json)

无模型复现捕获了“持有stdin时，daemon buffered reader导致解释器退出abort”的签名；新的raw-fd/select监听独立停止并join，五个修复后场景通过。旧v2丢弃了stderr，因此该机制不能被当作旧signal6的唯一已证原因。v4复现曾因所选JSON深度在当前解释器不触发异常而失败，保留原判；v5用明确标为合成的解析异常注入补齐该路径。[v5无模型结果](../runtime/goal-baselines/20261106/asr-startup-next/model-free-v5-first/report.json)

## 后续

继续验证ASR先准备、TTS后准备的共享GPU顺序；当前成功运行主要覆盖TTS先准备。完成当前模型组合的30分钟双语/记忆联合测试，以及真实麦克风、AEC、按键到静音、自然度、浏览器和现场跟踪验收。教学首句仍明显超时，将以保持来源校验的受控模型路径对照继续优化；实体PTZ仍暂缓。
