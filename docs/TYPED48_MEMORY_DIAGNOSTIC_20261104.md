# 48 条类型记忆：独立诊断方法

本片新增六条 `context_constraint / condition_limit` 的真实 Store 投影，不修改已冻结的 42 条投影、编码、Store probe、方法或结果。原 challenge v2 的 60 来源、96 查询和最小充分证据标签也不改变。新一轮不得继承旧 55/60 或原 raw-60 的成绩。

新 `typed48_common.rs` 只供新诊断复用冻结 fixture 读取、确定性类型映射和真实 Store 重建；返回持有临时 Store 的投影对象，后续向量导入与真实召回可直接复用，不再复制一套重建函数。它不挂生产库，不改变默认召回或自动晋升。

## 投影合同

六个原主题各导入十条事件，保留原 event_kind、原文、support_level、非空 accepted；不伪造偏好、已完成或提示效果字段。每主题八条合法候选分 5+3 两批走 enqueue/claim/stage/apply，合计 12 个合成任务，不生成模型开始回执。

48 条包括观察 12、回答证据 12、已接受待做计划 6、更正 6、学习目标 6、任务条件与推断边界 6。仍排除六条未接受计划及六条临时请求；它们在原来源中保留，但不能假称已接受或长期偏好。每条实际 memory.content、类型、范围、namespace/owner、epoch/status、来源原文/版本/链接和完整教育元数据均检查；随后再次读取 48 条 live evidence。

原 42 条正文与教育元数据、192 条查询输入必须逐字等于 `typed-projection-first`。新增总输入为 48 + 96 + 96 = 240。第二查询条件仍是原问题、空格、原目标、末尾空格，不改写问题。

完整最小证据集合的可用性单独报告，任何集合缺失成员都不能裁掉后计成功。预期可覆盖正题仍为 60/72、跨语 18/24、多证据 11/11；新条件来源增加部分完整替代集，而非把未接受计划或临时请求问题改标负例。这是来源可用性，不是实际检索成绩。原 12 个 scope control 只按原文件 hash 保留，本片不伪报已执行。

未提交向量时，embedding_work 只检查首批 32，不能重复请求后声称遍历 48。后续真实 Store 向量片必须通过 32+16 的 CAS 提交覆盖全部 48。当前活动仍映射为 `modular_rover`，不声称已有六种真实活动授权隔离。

## 编码合同

`typed48_encode.py` 默认仅验证显式 report SHA、240 条输入字节 hash、投影源码/方法/公共模块、原 fixture 全部文件、逐条实际 source 与 memory provenance。只有显式 `--run-model` 才调用既有 hash 固定的 `bge_m3_shadow.tokenize/encode`。复用旧 encoding utility 的 hash/JSON/RSS 函数；不修改旧 validator/main，也不通过补写旧报告来伪装 42 条流程。

模型维持官方固定 BGE-M3 native FP32，1024 维、CLS + L2、无查询或文档额外前缀，CPU 两线程/interop 一线程、batch 1、eager、eval/inference_mode、local_files_only、trust_remote_code=false、weights_only。所有 240 条输入重新编码，不混用来源不同的向量；资产先按已有固定 revision/hash 校验，不下载、不新增依赖。保存 token IDs/masks、向量 f32LE hash、逐条耗时与整进程 RSS 采样，不保存巨大 hidden tensor。编码成功不能称作已写入 Store 或检索通过。

## 根代理执行

```powershell
cargo test -p ai-tutor --bin typed48-projection-probe
cargo build -p ai-tutor --release --bin typed48-projection-probe
target/release/typed48-projection-probe.exe --root 'C:\Users\RBMS\Desktop\AI TUTOR' --output runtime/goal-baselines/20261104/typed48-projection-first
```

取得实际 `report.json` SHA 后，先运行无模型检查；示例中的 `ACTUAL_SHA` 必须替换为实际报告 hash：

```powershell
python scripts/memory-lab/test_typed48_encode.py --root . --projection runtime/goal-baselines/20261104/typed48-projection-first --projection-report-sha256 ACTUAL_SHA
python scripts/memory-lab/typed48_encode.py --root . --projection runtime/goal-baselines/20261104/typed48-projection-first --projection-report-sha256 ACTUAL_SHA --output runtime/goal-baselines/20261104/typed48-validate-first --validate-only
```

随后由根代理安排 WSL CPU 无竞争窗口，使用已有 `runtime/speech-gpu/omni/bin/python` 执行同一个新脚本，目录参数改为 `/mnt/c/Users/RBMS/Desktop/AI TUTOR/...`，新输出 `typed48-encode-first`、显式 `--run-model` 与真实 `--load-note`。本代理没有执行模型。

## 实际 Store 检索入口

新 `typed48_store_probe.rs` 复用 `typed48_common` 在新的自有临时 Store 重建全部 48 条，逐项比较已编码投影的正文、kind/scope、grade/support/importance、源原文/版本/状态。新 ID 与时间可以变化，真实重建 evidence 全部保留在新报告；任何正文或来源含义变化都会中止，不重新编码或修补输入。

程序核验显式 projection/encoding report SHA、报告内输入和向量 SHA、固定方法和 source hash、BGE profile、全部 240 个 key/text hash/1024 维归一化 f32LE 向量。通过实际 embedding_work → CAS 完成 32+16，确认 48 条读回向量和正文逐值匹配、无剩余工作。比较当前问题与问题+目标两条件的真实 legacy 词面、实际 eligible 纯 BGE、真实 BGE+legacy RRF。纯 BGE 保存完整 48 排名；Store 返回最多五条，报告只声称观察到该范围的同分。

实际排名结束后，用 `revise_education_event` 撤销新增的 `evaporation_s03` 条件来源，确认旧 evidence 无效、learner epoch 清缓存、迟到 CAS 拒绝且旧记忆不再召回。报告明确索引与排名是撤销前快照。这一次没有重跑跨 owner、forget 或原 12 项范围控制，不能继承前测作为本次已执行。

根代理在注册并编译新 bin 后运行，两个 SHA 都必须来自本轮真实输出：

```powershell
cargo test -p ai-tutor --bin typed48-store-probe
cargo build -p ai-tutor --release --bin typed48-store-probe
target/release/typed48-store-probe.exe --root 'C:\Users\RBMS\Desktop\AI TUTOR' --projection runtime/goal-baselines/20261104/typed48-projection-first --projection-report-sha256 PROJECTION_SHA --encoding runtime/goal-baselines/20261104/typed48-encode-first --encoding-report-sha256 ENCODING_SHA --output runtime/goal-baselines/20261104/typed48-store-first/report.json
```

## 冻结来源与验证状态

| 文件 | SHA256 |
|---|---|
| typed48_common.rs | `2dfcf8b95c399d8194aa944ff8ff8330e30365be0f0026c8ce3e42b4a52e98e8` |
| typed48_projection_probe.rs | `9a3b0186c75744660fa3ff3df45863258143f58d672b81b34b96e8835a6b9395` |
| typed48_projection_tests.rs | `3eca2896a77d847a45022778b5990bd3ef24fd668eb6b1bd22b23c848e27b665` |
| typed48_projection_method_v1.json | `3e9a977488da598ab79b0daf3b5b98988a68249c26d9fe68b87aa4aaffa05b2c` |
| typed48_encode.py | `4925d684b2b94dfa9650b2803945715d20c18d7dc96fcf686d0da4c065c2c9dd` |
| typed48_encode_method_v1.json | `025115dcbcd7bac73e3c68d0811dfa85c14742de8d6c6a3a4f296c23c180903a` |
| typed48_store_probe.rs | `c3f9f4bb76edeea264590a1c20fbbe63407e0f0d6d047b39657f39e8a92a8f60` |
| typed48_store_method_v1.json | `1aae0271537e1e111bbb07b53eee5a863f977c8cc2215e069438c004648e37c6` |

Rust 三项 projection 测试覆盖真实 48 条晋升及旧输入一致、映射不伪造、最小集合完整性，已由根代理通过，并纳入 workspace32 的 717 通过、0 失败、6 忽略。实际 release projection 已退出 0（进程墙钟 0.481 秒）：`runtime/goal-baselines/20261104/typed48-projection-first/report.json` SHA `8f36bf7d19f598c2ac6ee7eeb06e825915d556f84fbf77d01da3961381acfe65`；240 行 `encoder_inputs.jsonl` SHA `6f003d9ad4d2fdf14f0978f683b9dd5487ef527e362a54d1f40cf4b23d6cc7e7`。

实际可用完整替代集从 75 增至 82，正题可覆盖仍为 60/72；cross 仍 18/24，multi 仍 11/11。根代理 `typed48-validation-1.log` 已验证合法实际报告及九种篡改全部拒绝（10 项）；`typed48-validation-2.log` 与 `typed48-validate-first/report.json` 确认 240 输入、model_executed=false、store_vector_imported=false。新增 Store 入口的一项整条预算/最小集合回归已随 workspace33 通过；该轮总计 718 通过、0 失败、6 忽略。Store bin release 构建 18.149 秒，实际进程退出 0、墙钟 2.675 秒，内部计时 2.494 秒。

本代理未运行 Cargo 或模型。实现阶段的既有代码文件修改仅为 Cargo.toml 的两个新 bin 注册，备份于 `runtime/goal-backups/20261104/architecture_audit/typed48/` 和 `typed48-store-registration/`；结果文档更新前另备份于 `typed48-result-doc/`。

## 首次实际编码与 Store 结果

根代理执行全 240 条 CPU 编码成功，`typed48-encode-first/report.json` SHA 为 `1c1b261e4ed3d74d14fbacc713a9f0e2c32bbeca6cf03f1368c29024a3fa1a44`，向量文件 SHA 为 `01ad3da86852d64bab2c8d21b366b8e3bc0a74f170a15660d45320b3aa7c63e9`。整个诊断 128.444 秒，其中模型 load 8.638 秒、逐条 inference 合计 31.575 秒；其余包含资产校验、分词与输出，不应混为纯推理时延。最大所见整进程 RSS 为 2,323,591,168 字节，约 2.164 GiB。`VmHWM` 在卸载后略降，报告已标明采样异常；这是整进程采样事实，不是精确单调峰值或模型独占显存/RAM，亦不是平衡性能基线。

实际 Store 报告是文件 `runtime/goal-baselines/20261104/typed48-store-first`（无扩展名），SHA 为 `b058a6922b0790090ea8003607ca8be1ed5a3803952c1a3552e55c542d54d6bd`。首次调用把该文件名传给 `--output`，没有改名或重写报告字节。48 条通过真实 CAS 的批次为 32+16，48 条正文与向量读回逐值相同、eligible=48、remaining_work=0。实际修订新增条件 `evaporation_s03` 后，旧 evidence 失效、owner epoch 清空向量、迟到 CAS 被拒绝，控制通过。本次没有执行其它 owner、forget 或十二项 scope control。

下表分子为前五条完整命中至少一个原最小充分证据集合的题数。原正题分母始终为 72；48 条来源可覆盖的子集仍为 60。跨语原分母为 24、可覆盖 18，多证据分母为 11，不能把未覆盖问题从原分母中消失。

| 查询条件 | 真实 legacy 词面 | 纯 BGE | 真实 BGE+legacy RRF | 纯 BGE 跨语 | RRF 跨语 | 纯 BGE 多证据 | RRF 多证据 |
|---|---:|---:|---:|---:|---:|---:|---:|
| 当前问题 | 25/72（25/60） | 57/72（57/60） | 57/72（57/60） | 15/24（15/18） | 15/24（15/18） | 11/11 | 11/11 |
| 问题+原目标 | 24/72（24/60） | 57/72（57/60） | 53/72（53/60） | 15/24（15/18） | 14/24（14/18） | 11/11 | 8/11 |

两条件三种方法的 24 个负题均返回候选，不构成拒答通过。所有候选仍需相关性审查；本轮没有下游回答正确性或拒答测量。原始 60-source BGE 门槛与先前 42-source 成绩均不能继承为本轮通过结论。

## 独立复算与 42→48 配对

`typed48_store_audit.py` 只用 Python 标准库读取已冻结报告/向量，重新核验原 fixture hash、全部 192 条原查询与标签、原完整集合可用性、整条 5 条/2400 Unicode 字符预算、每题与所有主题/跨语/多证据汇总。它还从实际 1024 维向量独立计算全部纯向量 cosine；与 Rust 数值的最大差为当前问题 `1.077e-7`、问题+目标 `1.251e-7`。没有重新调用模型或 Store。两条件所有方法均无字符预算跳过；失败来自排名/来源可用性，而非截断正文。

复算输出 `runtime/goal-baselines/20261104/typed48-store-audit-first/report.json` 的 SHA 为 `0a8be5d278607e053bddc86292bbd7d331944ea90bd3b03f09a01be753d40ffb`；脚本 SHA 为 `86ed7e29e15d6c4194ff49567e045645203d4136a0fee3eb77825ab78baf992c`。与旧 42 编码共享的 234 条文本 hash、token 输入和向量逐值全部一致，最大坐标差为 0。因而本轮可以在固定查询/旧文档向量下，通过离线删除新增六条来隔离候选池变化；这个 42 子集只是离线配对，不是新一次 Store 执行。

- 纯 BGE 当前问题：旧 42 实际 55 → 新向量的 42 子集 55 → 新 48 实际 57。新增完整命中 `germination_q02`、`evaporation_q02`，依靠新增条件来源构成的原有合法替代集，无完整命中退步。
- 纯 BGE 问题+目标：55 → 55 → 57。新增完整命中 `paper_bridge_q02`、`evaporation_q02`，无完整命中退步。候选池增加没有新增可覆盖题数，而是使原本可覆盖问题更容易找到充分依据。
- 真实词面当前问题 21→25、问题+目标 20→24，各四题新增命中、没有完整命中退步。真实 RRF 当前问题 55→57；问题+目标 52→53，有四题恢复、三题退步（`fractions_q08`、`fractions_q11`、`evaporation_q12`），不能只报净增一题。

实际 RRF 的跨次比较同时含重新生成的 memory ID 同分决胜影响，不能把每项变化都归因于新增条件。新报告当前问题有 84/96 个查询在可见返回范围内出现 RRF 同分，问题+目标 78/96；纯向量两条件均无同分。未返回的 RRF 排名未保存，不推断不可见的第五/第六边界。冻结报告保留本次真实排序，不按标签修改 tie-break。

## 尚未解决的具体失败

纯 BGE 两条件都遗漏三道可覆盖的跨语问题：`germination_q06` 要找独立回答及保持条件的证据（所需 `germination_s06` 排名 25/9）；`evaporation_q06` 要找独立提出读量筒方法及推断范围（`evaporation_s06` 排名 7/8）；`evaporation_q08` 问是否未经提示提出初始读数（`evaporation_s05` 排名 6/8）。斜线分别为当前问题/问题+目标排名。新增任务条件不替代“原回答是否经提示”的证据，跨语与提示依赖区分问题仍在。

问题+目标的实际 RRF 相对同轮纯 BGE 丢失四题：

| 问题 | 必需但未返回的来源 | 纯 BGE 排名 | RRF 候选边界 |
|---|---|---:|---|
| fractions_q08 | fractions_s05 | 3 | 其它主题词面候选与语义第一条进入前五；所需中文原回答未返回 |
| fractions_q11 | fractions_s02 | 4 | 返回 s01/s09 仍不足；两种原最小集合都还要求 s02 |
| fractions_q12 | fractions_s05 | 4 | 只返回 s06，双来源集合缺少未经提示/先前回答一侧 |
| evaporation_q12 | evaporation_s05 | 4 | 返回 s06 与新条件 s03 仍不能代替原 s05+s06 集合 |

这四题表明旧词面融合、目标拼接与前五候选边界仍会损失完整证据。它们不是向量入库失败，也没有被来源可信度或高相似度自动判作事实。新增条件类型与缓存失效链已得到本轮证据；默认召回和自动晋升保持原状态，下一步应单独评估相关性/拒答与融合候选策略，而非改写本批标签或宣布生产通过。

复算命令（输出须使用新路径，脚本拒绝覆盖）：

```powershell
python scripts/memory-lab/typed48_store_audit.py --root . --store-report runtime/goal-baselines/20261104/typed48-store-first --store-report-sha256 b058a6922b0790090ea8003607ca8be1ed5a3803952c1a3552e55c542d54d6bd --encoding runtime/goal-baselines/20261104/typed48-encode-first --output runtime/goal-baselines/20261104/typed48-store-audit-repeat/report.json
```
