# 教学阶段生成合同

物理教学首批真实模型 8 例中，`static_measurement_en` 正文有用，却提出不存在的 `static_observation`。原生成校验只检查字段和字符串，直到 apply 时才会跳过非法阶段；该探针没有应用任何候选，所以原记录应保持协议通过、完整候选可用性 7/8，不能改写成当时已经拒绝。

本次将实际 `tutor::STAGES` 十项阶段同步给模型可见的 schema；条件仅作用于 stage，goal、pending_step 和 reported_result 仍是普通文本。schema 作为提示的一部分发送，不声称服务端强制解码。`validate_reply` 在字段、字符串检查后，用相同业务常量拒绝非法阶段，不把近义词自动改成合法值。现有 `apply_reply` 的阶段、来源、代次、计划门限保持。

非法阶段现在令整个全量回复或 ACP final 校验失败，和其他非法任务字段/引用的处理一致。流式前缀可能已经显示或播放，失败不能撤销已发生的投递；原账本保留实际已显示内容，停止后续声音，final 不标为成功且不提交其候选。这意味着服务端输出偶发违规仍可能表现为部分回复后失败；不能用静默修补或自动重试掩盖。

新增四项回归覆盖实际失败的三候选形状、真实临时 Store 的原有应用防线、十个合法阶段全集、非法大小写/空白、其他字段不受阶段枚举影响、schema 与业务常量同步，以及 SegmentDecoder 换行/干净 EOF 两种 final 拒绝。最后一项是 decoder/ledger 单元测试，测试显式 invalidate，不称为真实 ACP 集成验收；生产失败路径经独立静态审核，既有投递回归另保留。

阶段补丁先经独立只读审阅，再在 checkpoint20 完整冻结后应用。原草案和旧证据保持不变；真实源文件经过 rustfmt。基线备份为 `runtime/goal-backups/20261104/root/stage-contract/`，独立审阅为 `runtime/goal-baselines/20261104/stage-contract-review-independent.json`。

模型选择和两次调用上限在运行前单独冻结于 `stage-contract-model-selection.json`；准备器保留原八项场景，只有预声明中英两项被选择，其余六项不计为本次通过。

## 本片实际验证

定向真实 Store/校验集成测试 3/3 通过；随后 workspace46 为 **829 通过、0 失败、9 忽略，含 UI 128，无 warning**，116.279 秒。原生全部 bin release 构建成功，170.932 秒；新程序的隔离模拟库 HTTP 检查 1.590 秒通过。没有为本片重新执行相机/语音长测、浏览器、声音或运动。

296 项源码在完整 workspace/release 构建前快照，测后 SHA 全部一致；精确二进制另存 `checkpoint21-measured-binaries/`。独立审阅接受原补丁的生产失败处理路径；补丁实际应用时恢复 schema 原有末尾无换行格式后精确匹配拟议字节，再对 Rust 文件 rustfmt，格式化后的真实 SHA 才是本片构建基线。没有用草案 SHA 冒充实测源码。

真实模型产物在 `runtime/goal-baselines/20261104/physical-instruction-stage-contract-second/`。中英各一次首次调用，无重试/候选应用/显示/播放/设备，各自正常 close 且 running=0；外层耗时 19.410/22.685 秒。两个结果均非空提出合法 `stage=observation`，协议及清理 2/2，五段正文都提供了条件明确、实际有用的静态测量指导。首段消费者观察为 12.097/14.858 秒，不是显示或音频时延，未达实时目标。

**完整正文与所有候选可用性为 1/2，不是 2/2。** 方法知情的独立审阅接受中文全部三项任务候选及一项当前活动记忆；英文三项任务候选有一项失败：reported_result 写成 “the stopping distance has not yet been measured”，而唯一来源只说学习者想测距离，没有明确证实此前尚未测过。应保留“本次尚未提供测量值”的未知状态，不能把没有报告扩写为没有发生。该失败未应用、未补抽、未回写原评分。英文 final 的 `edge.Record`、`used.What` 缺空格另记可读性问题，不混为阶段合同失败。

原始八例仍保持 checkpoint20 结果。本片两条新结果说明模型此次使用了正确枚举；不能推断模型总体错误率下降，不能声称真实模型触发过非法阶段拒绝，该拒绝由上述确定性回归验证。准备的独立审阅发生在首次调用已开始之后，文件明确写为事后核对，不冒充测前 gate；根代理另有实际测前哈希核验。

候选增量会增加运行时读取的 `implementation-package/AI_Tutor_Codex_Implementation_Package/schemas/tutor_reply.schema.json`，与新 exe 成对交付。回退本片须恢复该 schema 及 checkpoint20 二进制；schema 原文保存在上述基线备份中。安装 exe 不替换，本片没有改变教学 policy。下一片需统一处理“未知不是否定”以及历史答案的提示情境/数值/纠正保真，不能仅靠合法枚举或引用 ID 宣称教学质量合格。
