# 类型化记忆投影诊断

第十一检查点实跑补充：4项Rust投影测试及7项Python文本合同测试通过。`runtime/goal-baselines/20261104/typed-projection-first/report.json`真实投影42/排除18，42条live evidence均重新核对、12个job已apply、快状态未改变；报告SHA256 `5feb84ddc072b05a8bc9106082bfec096f8f5ec3208403f0ca9c018cca19e426`，编码输入SHA256 `d93cfa39f184e51359837ad6ed67f31688ada1a54a8c8d5f57df447df38e2368`。

只读验证通过后，`typed_projection_encode.py`调用原hash固定BGE helper，CPU FP32/2 intra线程/1 inter线程，对42条实际记忆正文和192条原查询完成234条1024维归一化编码。`typed-encode-first/report.json`状态为 `encoding_completed_no_retrieval`，SHA256 `a1545f053fc843ff6aeee0c274790b83e66aeb6032d15a4bd04845b11b2a5a7d`。token数10–73，无截断；模型加载8657.914ms、逐项编码合计30885.450ms、诊断总124.033秒（含核验/导入等），不作为冷启动或平衡性能基准。最大观测整进程RSS 2,321,285,120字节，观察到HWM下降，未包装为独占/精确峰值。向量、token ID与文本hash完整留存。

**这些新向量尚未导入Store检索，检索/回答/拒答质量未评测，生产仍不启用。** 原raw60与raw18结果不适用于本片42条有类型的实际正文；也没有把确定映射候选的通过当成LLM抽取正确率。下方描述为最初投影设计，编码执行状态以上述实跑补充为准。

本片在独立合成数据库中使用现有真实 Store 类型化晋升接口，准备后续本地编码的精确文本。没有运行编码、检索或模型抽取；生产类型、默认召回和原挑战数据均不改。

## 固定输入与合法覆盖

使用已冻结 challenge v2 manifest `858ee969827d54d1687d1a3966e726576a66024f9b462683323732dde94fbb81` 的全部 60 条来源。候选由预先声明的类型映射产生，内容始终为原来源全文，不翻译、改写或合并。真实 `apply_memory_candidate` 添加生产已有的类型限定前缀，导出从 `memory_evidence` 读出的实际 `memory.content`。

| 原始来源 | 数量 | 本片处理 |
|---|---:|---|
| observation / reported_observation | 12 | trial_observation；据报告的尝试与观察 |
| learner_answer / task_specific_response | 12 | concept_evidence；仅本次回答证据、不代表掌握；保留 hinted 6 条和 independent 6 条 |
| plan / accepted_plan 且 accepted=true | 6 | accepted_pending_experiment；已接受、尚待完成，不填写已观察结果 |
| correction / explicit_correction | 6 | correction；保留原更正文句。原集没有被更正的错误旧事件，不虚构 supersedes 或废弃其他有效来源 |
| learner_statement / stated_goal | 6 | learning_goal；明确表达的目标，不当作完成或掌握 |
| learner_statement / condition_limit | 6 | 排除：当前没有对应的生产记忆类型，不改成回答、观察或目标 |
| plan / unaccepted_plan | 6 | 排除：未接受提议不能晋升为已接受待做实验 |
| learner_statement / temporary_request | 6 | 排除：本次请求不能晋升为持续帮助偏好 |

预期 42 条实际记忆、18 条排除。原数据没有明确持续偏好，故 `help_preference` 覆盖数为 0；提示后的回答记录也不等于已测量提示效果，`measured_hint_effectiveness_coverage` 为 0。未知组合明确列为不支持，不为凑齐 60 条而转换语义。

事件只复制原有 text、event_kind、support_level，以及原值非 null 时的 accepted；不添加 `explicit_preference`、`preference_scope` 等放行标志。候选引用原文、事件 ID 和版本，importance 四项固定为 0，来源等级由 Store 重新确定为 simulated，不由候选自行提升。

## 实际接口与产物

每个主题使用一个合成 Store session；每个 session 的七个受支持候选按不超过五个一批依次经过 `enqueue_memory_job → claim_memory_job → stage_memory_candidates → apply_memory_candidate`。共十二批，保持默认预算，无模型调用、没有 mark started 回执。六个原逻辑 session 与真实 Store session 的映射保存到报告；原逻辑 workshop 仍只在该 fixture 中映射到现有 modular_rover activity，这不是生产跨活动权限验证。

所有 42 条记忆在输出前再次通过 `memory_evidence` 读取并检查；编码输入是此时真实正文及其 SHA256。另通过 `embedding_work` 检查首批 32 条的 profile/维度/正文绑定。该 API 上限 32 且不支持未提交向量的游标分页，本片没有伪造向量使队列前进，因此明确报告首批覆盖 32、全部 snapshot 覆盖 false；这不影响 42 条正文的逐项 live evidence 核验。

独立可注册 bin：`typed-projection-probe`，路径 `../../scripts/memory-lab/typed_projection_probe.rs`，保留默认测试启用。由根代理统一运行：

```powershell
cargo test -p ai-tutor --bin typed-projection-probe
cargo build -p ai-tutor --release --bin typed-projection-probe
target/release/typed-projection-probe.exe --root 'C:\Users\RBMS\Desktop\AI TUTOR' --output runtime/goal-baselines/20261104/typed-projection-first
```

新目录包含：

- `report.json`：全部 60 条决策、原始来源、排除原因、真实事件/版本、memory evidence、support/source grade、类型数量、session/job 映射与方法/源码 SHA。
- `encoder_inputs.jsonl`：42 条真实 projected memory 和两种查询条件各 96 条，共 234 条。每条包含稳定 key、精确文本及 SHA；BGE doc/query 前缀均为空。`query_plus_goal` 保留原方法的末尾空格。

输出使用新目录；不覆盖旧结果。后续编码必须核对本次 report/input SHA 与固定 BGE profile，不能复用原 raw-source 文本的向量充作投影向量。此处 `source:<id>` 表示该来源映射出的真实记忆，其正文已含生产类型前缀；完整报告的来源映射用于原标签计分，标签本身不改。42 来源结果与原 raw-60、原 raw-18 Store 结果分别保存，不继承它们的分母、质量门槛或通过结论。

## condition_limit 的生产缺口建议

六条不支持来源确实包含教育上有用的条件与解释边界：未控制水量/跨度/口味/气流、未测温度，阅读原文与推断的区别，以及程序初值不同带来的解释限制。它们不全是实验观察，也不都是学习者回答，现有 `concept_evidence` 的 learner_answer 门槛不应为本集放宽。

后续最小可讨论项是独立的“本任务条件与解释限制”类型，建议候选名 `task_condition_limit`、claim `reported_condition_limit`，限定 current_activity；正文保留原句并加“据本任务来源记录的条件与解释限制”前缀。来源需为有版本的明确 learner_statement/observation 或教师记录，保留报告等级；计划、助手推测、摘要不能充作已发生的条件。不自动写 completed、reported_result、掌握程度或持续偏好，也不把报告中的解释限制升级为普遍科学定律。相同来源失效与 epoch/CAS 规则应继续适用。

这只是新增生产类型的建议，本片未加 kind、未放宽任何门槛。后续若实现，应先补“条件被更正后失效、未测不变已测、计划不变观察、阅读推断不变事实、不同初值不误判循环错误”的独立用例，再生成新投影版本；不能修改当前冻结输出后沿用旧 hash。

## 冻结与验证状态

- 方法 SHA256：`3b059ba50765be0a7a54904654ed8efd654a184d6fa9c573746e9ca8a792f8de`。
- Probe SHA256：`b76c7ef34f2b16c8eb95b7dec9d8eb2c57cd14daac8240042dd11f5c8c1c1277`。
- 独立测试 SHA256：`7dc1a98be3c959f9f5b20541ce77dbef138f75063909cdfe77d018ace723c325`。
- 已写四项真实 Store 测试：全部 42/18 与状态不升级、hinted 不能冒充 independent、未接受计划/临时偏好不能晋升、错误引用/版本拒绝及更正失效。
- 本代理仅运行 rustfmt，未 Cargo、编码、排名或设备；编译和实际投影结果待根代理统一验证。650 测试结果不覆盖本新增探针。

## 独立 BGE 编码准备

新增 `scripts/memory-lab/typed_projection_encode.py`，复用 SHA 固定的原 `bge_m3_shadow.py` 的 `tokenize`、`encode` 和内存采样函数，仅使用 BGE 分支。原 raw-60 脚本、方法与报告保持不变。新方法为 `typed_projection_encode_method_v1.json`，在任何本次编码前冻结。

真实投影报告尚待根代理执行，因此方法不预填不存在的结果 SHA。调用者必须提供 `--projection-report-sha256`，脚本核对该已完成报告的精确字节，再核对报告声明的 inputs 文件 SHA、投影方法和 probe SHA、原 v2 的全部来源/查询/标签/control 文件。42 条正文逐项与真实 memory evidence、类型前缀、原来源/版本/support/grade 相核对；192 条查询与原 v2 逐字比较，包含 question+goal 的末尾空格。删项、重复、重排或自行重算局部 hash 后改词也不能通过。

先在新输出目录运行 `--validate-only`（不加载 tokenizer 或模型），通过后由根代理在无并行模型/构建负载窗口安排 `--run-model`：

```text
<已有 WSL omni Python> scripts/memory-lab/typed_projection_encode.py
  --root <WSL 项目绝对路径>
  --projection <真实 typed-projection-first 目录>
  --projection-report-sha256 <根代理实际核对的报告小写 SHA256>
  --output <新的编码结果目录>
  --run-model
  --load-note <本次实际后台负载说明>
```

模型固定为现有官方 BGE-M3 revision `5617a9f61b028005a4858fdac845db406aefb181`，复核八个本地资产大小及 Git/LFS 哈希；不下载、不安装。CPU FP32、batch 1、2 个 intra/1 个 inter 线程、eager attention、eval/inference_mode、CLS+L2，`local_files_only=true`、`trust_remote_code=false`、`weights_only=true`；超过 512 token 直接失败，不截断。

保存 `token_inputs.json`、全部 234 条 `bge_m3-encodings.json` 和 `report.json`。向量保存完整 FP32 数值与 little-endian SHA、token IDs/mask、输入文本 hash、每条 tokenization/inference 耗时；只保留 hidden shape，不保存巨大 hidden tensor。报告分开记录资产验证、模型加载、总编码耗时，并保存 load 前后、encoding 后、unload 后和结束的整个进程 RSS/HWM；HWM 若下降会明确记录。最大所见 RSS 不称为模型独占峰值或精确单调高水位。

本片只编码：`store_vector_imported=false`、`ranking_executed=false`。真实 Store 向量导入和候选排序是下一片，不从编码成功推导检索、拒答或生产验收通过。

- 新编码方法 SHA：`18404e264f84bb6576e5dd88fc4b5431134b3608e05f717024808290e7f64689`。
- 新编码脚本 SHA：`707ef0cbb2512723b99c751e8df619856cf382b429559d500b92501e765625e5`。
- 纯 JSON 合同测试 SHA：`34b361e5d3c618880995eed3efee3faf60d59957d3a9351d339ffb6e5ed36049`，包含 7 项，使用明确标注的 mock JSON，不冒充真实 Store 结果。
- 已完成 Python AST 与 JSON 语法检查；本代理未运行测试、模型或 tokenizer。实际投影、校验及编码结果待根代理记录。
