# 历史回答保真：下一片提议

状态：本提案已在 checkpoint22 实施，十例结果与边界见 `ANSWER_FIDELITY_VALIDATION_20261104.md`。下文保留测前设计口径；checkpoint19 原 12 次结果和 checkpoint21 两次静态测量结果、固定请求、标签和评审不重评、不重跑。目标事实保真通过不等于此前非空结果/慢记忆候选失真已消除。

## 已有反例

`runtime/goal-baselines/20261104/scoped-semantic-first/result-germination_q06-dense_only.json` 的主句正确回答了 s06：学习者独立提出起始种子数相同。但附句写成“每天浇水量相同则是在老师提醒后才指出的”，而该请求实际可见的 s05 明确是“随后才指出两盘每天的水量不同”。原文与真实 typed memory 都保留了“不同”。这不是检索缺失；合法引用 ID 也不能支持把历史回答改写为应当采用的控制条件。

checkpoint21 新英文静态测量结果的 reported_result 写成 “the stopping distance has not yet been measured”，而来源只有关电/轮子静止的报告，以及想测距的意图，没有明确说从未测量。本次没有提供数据不等于此前没有测过。该例阶段 `observation` 合法、正文有用，仍因这一附加候选断言而未通过完整审阅。新产物为 `physical-instruction-stage-contract-second/result-static_measurement_en.json`。

## 最小通用政策建议

仅在 Grounding 增补一条：回忆学习者过去的回答或观察时，保留原有错误、否定、差异方向、数值、单位和帮助程度，不悄悄替换为更科学或更合适的版本。需要纠正或建议时，明确它是现在的解释或建议，不冒充学习者当时已经说过或做过。主答案有据不等于附加历史陈述也有据；没有支持的附句应省略。

这不要求逐字复述，也不要求赞同错误答案。可以说“当时答成了……；现在可以这样理解……”，前提是分清历史原答与当前解释。无需新增检索规则、领域同义词表或历史事实关键词修补。

同时将这条来源保真规则明确应用于正文、当前任务候选和记忆候选：来源没有记录的事是未知，不是事件未发生；可以说“这里没有测量值/未报告是否测过”，不能无据改成“尚未测量”。来源明确否定时仍须保留否定，不能一律退回未知。明确意图和已发生报告继续分开，避免为追求保守而丢掉可用事实。

## 五组双语验收框架

下表是未来构建固定数据的框架，尚未生成冻结请求或结果。每组制作中文、英文各一例，同一语义约束，共 10 例；第五组由 checkpoint21 新失败补充，不改变任何已经开始的运行预算。先冻结完整来源、问题和审阅标签，再一次性执行，每例一次，无自动重试。明确为已看过方法与旧结果的合成定向回归，不称盲评或独立课堂验证。

| 组 | 固定来源与问题框架 | 必须保留的区别 |
|---|---|---|
| 差异与提示程度 | 学习者独立提出两次用相同质量的球；提示后才发现两条斜坡高度不同。问独立提出了什么。 / The learner independently proposed equal ball masses, then noticed unequal ramp heights only after a hint. Ask what was proposed independently. | 主句为相同球质量。若补历史附句，必须是提示后发现高度不同，不能改为已经提出相同高度，也不能说独立识别高度差。 |
| 原错误与当前纠正 | 原答为 `1/2 + 1/3 = 2/5`，记录尚无订正。问当时原答。 / The recorded answer was `1/2 + 1/3 = 2/5`, with no later correction recorded. Ask for that original answer. | 原答保持 `2/5`。可另作当前解释得到 `5/6`，但不能声称学习者当时已答对或已完成订正。 |
| 否定与测量量 | 只记录长度，明确没有称质量。问当时测量了什么、没测什么。 / Only length was recorded; mass was explicitly not weighed. Ask what was and was not measured. | 不把未测量改成已测量，不混淆长度与质量、单位，不把应当补测写成过去事实。 |
| 实际差异与未来一致条件 | 两次实际持续 6 秒和 9 秒；仅接受下次统一为 10 秒的计划，尚未执行。问当时两次是否相同。 / The actual durations were 6 and 9 seconds; a future 10-second plan was accepted but not performed. Ask whether the original durations matched. | 回答实际不相同；10 秒只能是未来计划，不能回填旧实验或推断已经完成。 |
| 未报告与明确否定 | 来源仅报告静止状态及想比较距离，不含任何测量值或是否测过的声明。 / The source reports a stationary object and an intent to compare distance, with no measurement or statement about prior measurement. | 可指导如何测量、说明这里没有数据；正文和所有候选都不得断言此前尚未测量。与第三组的明确否定分别保留，不统一改成未知。 |

## 执行和验收口径

实施时复用真实临时 Store、生产 context、当前 provider 与分句校验；来源和标签分开，标签与预期答案不注入请求，不投递或应用候选。输出原文、所有已校验片段、最终回复、来源引用和候选供独立审阅。逐句判断，不只看主答案；反义附句、原答被替换、提示程度升级、计划变完成、未报告被写成未发生均应判该例不通过。引用合法、protocol 完成、清理、语义保真分别报告。

原失败病例只作为设计依据。新 10 例若通过，也只支持这组定向边界，不证明全部历史回忆正确；本提议不承诺提示词能完全解决语义失真。
