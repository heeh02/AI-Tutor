# diagnostic_postfreeze_v1 实际数据与静态验证

实际 fixture 已写入 rust/ai-tutor/tests/fixtures/diagnostic_postfreeze_v1/，冻结时间为北京时间 2026-11-04 04:52:47。保留已冻结规范及词面方法原文件。数据由已看过方法和 v2 结果的代理编写；不是人工标签、盲评、随机独立测试或真实学习者记录。没有调用模型、运行检索比较、修改生产默认或 Cargo。

## 文件和冻结哈希

| 文件 | 条数 | SHA-256 |
| --- | ---: | --- |
| sources.jsonl | 60 | b96d0232e8927ae676345f9463e87a641160c0256a0d16a0c3aad60005da55fd |
| queries.jsonl | 96 | 79e835b504ac045da63f8ab74f15de1ccfd1faeb0981a7fa6dc026c399abbbaa |
| labels.jsonl | 96 | 75287aba94518b0b50dc78ea37c81231996999991bc8675a6a2780e02677c20e |
| scope_controls.jsonl | 12 | 7fffe45855e063a5e5e35748ee9e118d9e57ab24b2fe017acbe132d2459238cf |
| record_schemas.json | 四种记录 schema 及子类型 | 0209f75e8c108bd34e8a49b96dbcecc84f30c7032cac70d8a2ecbb9ca484f394 |
| manifest.json | 1 | a0af509b5ae8a5cd25b12b4ad7491c42104239f563e8e3627959d73bb3aabbc6 |

方法 SHA 为 5296ca5b5116e213b020b9aabe68f6d524926e8ba4b69649bf6e706fce65e18c；规范 SHA 为 14d04db43e70cd24e84063d53756baa64bda4e9e7736a592f2c0e34d67cb982c。作者源码 scripts/memory-lab/author_diagnostic_postfreeze.py 的 SHA 008c7199f4060d0cdfcb103df6e347645cdf7a4607fd5a2520a1e1f0dca6ef8c 也存于 manifest。新输出目录存在时拒绝覆盖；时间戳使重建的 manifest 哈希不同，四份数据内容来自固定作者源。

## 内容与标签边界

六主题各有 10 条来源、12 个可回答问题、4 个无对应事实的问题：种子发芽、纸桥承重、配方分数、阅读推断、循环调试、蒸发测量。总计 72 正例、24 负例，另有 12 个范围/失效控制，未把控制并入正例分母。

问题语言为中文 30、英文 30、混读 36；24 个单一跨语言目标及 12 个必须结合中英文来源的双证据问题单列。数字形式包括种子枚数、成功承载与失败阈值、3/4 与 1/4、人物钟点、循环次数与实际输出、mL 与 g；条件包括水量/光照、折法/跨度、口味/配比、初值、形状/气流。提示依赖和未执行计划有明确原文。

12 个问题带替代最小集合，例如有效观察或对应的有效更正都能提供同一事实；双证据比较允许“观察 A + 观察 B”或“更正 A + 观察 B”。更正是当前有效来源，不把旧错误原句列为有效来源；替代条目也不提高独立证据数量或可信度。

“建议尚未接受/执行”“已经同意但尚未执行”“还未完成目标”都是可回答问题。负例缺少所问具体事实，如考试分数、准确仪器/测量值、永久偏好或无关任务信息。某些来源明确说“没有计时/称重”，可以支持解释为何不提供数值，却不提供所问数值；它们列为 context_only。不能把无数值证据说成完全没有可引用的限制说明。

正例有 claim_checks、禁用结论及引用。引用采用整个原始来源的精确 Unicode 字符范围，保留否定与条件，不摘掉“未”“提示后”等限定词。它验证原文和版本存在，并不自动证明自然语言蕴含或穷尽全部可替代推理路径。标签属于作者诊断性标注；后续审阅若发现遗漏的充分组合，应在看检索成绩前追加勘误版本并保留本版。

每题都面对同一个 60 条合格来源池，不能按 pack 预筛到 10 条。逻辑范围为 simulator 的跨主题 workshop，不映射成产品 modular_rover，不声称验证了通用活动模块或实际 Store 接线。12 控制覆盖其他 learner、namespace、activity、失效来源、错误版本、同文异主，各两条；同文异主再次验证 learner 门禁，不算第六种独立授权机制。

主题间重复使用来源角色与题型槽位，ID 也保留作者顺序，这是合成集限制。未来应报告并列位次，不根据题号、ID、mode、tags、答案标签或 pack 选答案。排名器只接收规定正文及当前范围；来源类型用于约束和解释，不能把标签或 claim_checks 拼入检索文本。派生翻译必须回链原 source ID，不能重复计分。

## 已执行的静态检查

scripts/memory-lab/check_diagnostic_postfreeze.py 仅用 Python 标准库，验证固定 schema 字段、ID/文本去重、数量和语言分布、每题相同候选池、来源状态/owner/scope/version、事件与 claim type、引用范围和原文、集合去重与冗余超集、多证据语言组成、负例无支持 claim，以及控制的单字段变化。它核对 JSON Schema 和检查器字段表及哈希，无需安装 schema 执行依赖。

首次报告 runtime/goal-baselines/20261104/diagnostic-postfreeze-static-first/report.json 的 SHA-256 为 86d159005000706166f1de4312d45ea59318a6bd88294845a694e0c9de11badf。结构和引用检查通过。10 种内存副本故意损坏全部被拒：重复 ID、错 owner、失效来源、错引文、错版本、冗余超集、单来源伪装多来源、无证据题附支持 claim、放行范围控制、删除控制。原数据没有被测试修改，也未逐字归一化复用 v2 文本或问题。

静态通过不等于语义正确、独立审阅或质量通过。报告保留 retrieval_run=false、model_called=false、semantic_quality_pass=false、human_annotated=false、blind_evaluation=false。作者首版输出前的自我审读修正了“盘内总数”不应擅称起始数，以及小岚时间题不应要求来源未命名的人物；均在首次数据冻结前完成，未查看检索输出。

从根目录复核（使用新报告路径）：

    python scripts/memory-lab/check_diagnostic_postfreeze.py --root . --fixture rust/ai-tutor/tests/fixtures/diagnostic_postfreeze_v1 --self-test --output runtime/goal-baselines/20261104/diagnostic-postfreeze-static-second/report.json

后续检索仍须单独执行，完整保留 96 行及 12 控制结果。manifest 记录 top 5、2400 字预算、问题/goal 两条件和预定门槛；不得看成绩后改阈值、词表、标签或删难题后仍称未见测试。
