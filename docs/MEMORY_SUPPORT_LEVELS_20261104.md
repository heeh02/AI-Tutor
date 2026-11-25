# 多来源记忆的提示情境一致性

本片仅收紧教育记忆候选的 `support_level` 准入，不改变来源等级、重要性、召回、教学回复或自动晋升开关。代码验证阶段未运行模型；之后完成了下述中英各一次合成 shadow 复测。未运行设备或真人评估。

## 已复现的缺口

原 `store/memory_policy.rs` 对非 `unknown` 的提示情境只要求至少一条引用来源匹配。两个同活动、合法且版本有效的 `learner_answer`，分别标为 `independent` 与 `hinted`，因此能被合并成一个宣称任一明确等级的候选。提示程度不同并不构成等级高低关系；把其中一个推广到整个候选会丢失另一条证据的作答条件。

根代理先运行新增真实 Store 回归，日志保留于 `runtime/goal-baselines/20261104/memory-support-levels-before.log`：终态 exit 1，32.339 秒，3 项通过、4 项失败。四项失败均为预期的错误准入：

- independent + hinted 被合并为 independent；
- independent + hinted 被合并为 hinted；
- independent + unknown 被合并为 independent；
- independent + 缺失字段被合并为 independent。

回归使用临时 SQLite、实际教育事件、入队、领取回执及候选暂存。三个正例进一步实际应用候选，核对来源 ID、版本、原始内容、提示情境、来源数量及模拟来源等级。它们覆盖同等级的多来源、混合来源保守标为 unknown，以及同一任务按来源拆成不同提示情境的候选。失败事务也不得留下另一条合法候选的部分暂存结果。

原生产源 SHA256：`6549cd5645876f9138a3e14e3315efcf50563614a55fefbc4421b46772f8e530`。修复前测试原字节 SHA256：`17bb2aa6f6dbc0eb0e3931d242b6ffb79c9e6daaa2d7deaa7144e73257cfb44b`。二者备份在 `runtime/goal-backups/20261104/architecture_audit/memory-support-levels/`；修复没有改动七项回归的断言或测试字节。

## 实现及范围

非 `unknown` 候选改为要求**全部引用来源**明确记录与候选相同的提示情境。原有非空、去重、来源完整性和版本检查仍先执行；暂存及应用都复用此校验。混合、未知或缺失的来源可以拆分为各自有据的候选，或使合并候选保持 `unknown`。不改写原事件，不按提示程度排序，不让重要性分数提高来源等级。`context_constraint` 继续固定为 `unknown`。

`memory_consolidation.md` 同步明确上述规则，提示版本从 `educational-memory-prompt-v3` 提升为 `educational-memory-prompt-v4`；响应字段和枚举未变，schema 仍是 `educational-memory-v2`。旧版本任务仍遵循既有 worker 的版本核对，不静默当作新提示任务。

根代理统一运行 workspace44，实际 **807 通过、0 失败、9 忽略**，包含 UI 128 项，无 warning，耗时 107.32 秒。七项提示情境回归全部通过，测试字节仍为上述修复前 SHA；四项预期拒绝由红转绿，三个合法路径保持通过。该结果证明本地准入边界，不代表模型整理质量或学习效果通过；模型证据单列如下。

## 后续模型核验可复用入口

已有 `memory-education-probe --real-synthetic --language zh|en --scenario preferences_and_support` 包含同一任务内的无提示错误答案及提示后更正答案，走生产 `MemoryWorker` 与 Store 暂存，每次单次合成调用、不应用候选。该场景用于审阅模型是否保留不同作答情境，不能保证模型一定提出混合候选；准入拒绝的确定性证据仍由上述 Store 回归提供。

## 提示 v4 双语实测与独立审阅

证据目录 `runtime/goal-baselines/20261104/memory-support-v4-first/`，方法 SHA `260a41c11ab36835f458b474a5788e6a1ad8755725e1cfedce2868697da647a3`。中文、英文各唯一一次调用，均 exit0，完整诊断分别 24.890、31.561 秒；中文 4 条、英文 5 条候选均为模拟来源 shadow，没有应用。

方法知情、非盲的独立审阅逐条核对了原四来源及全部 9 个候选：两语言均保留科学词汇英→中的持续偏好及可修改性；临时提示要求保持本次会话范围；原无提示错误答案 2/5 未被暗改；提示后 5/6 保留具体提示和未测无提示新题的限制。原三项必需覆盖每语言均 3/3，两项可选临时偏好也正确保留。没有把提示情境当作答案正确性、可信度或一般能力。

英文额外候选将 2/5 解释为“可能分别把分子、分母相加”，使用 `may tentatively reflect`、`revisable interpretation` 并限于该次作答。该算术模式与答案相容，作为明确可修正的假设可以接受，但没有证明实际思维过程，也不额外计入必需覆盖。

独立记录 `independent-review-ui.json` SHA `12b5d3d3b474219277894a990686b255c3cafa3db386fde66605e536c476387c`。本次未出现多来源提示情境混合候选，不能据成功输出声称模型运行触发过混合拒绝 guard；仍须引用真实 Store 回归。仅两次固定合成场景、模型身份与采样未独立冻结，不构成严格因果对照、广泛模型覆盖率、教育效果或真实学习者长期情境验收。旧模型结果和原评分未修改。
