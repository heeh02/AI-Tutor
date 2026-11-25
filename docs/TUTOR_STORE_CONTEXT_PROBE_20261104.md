# 真实 Store 活动续做教学诊断（2026-11-04）

新入口 `tutor-store-context-probe` 使用真实临时模拟 Store 和未改动的生产 `build_context`。诊断本身不修改旧 36 题、typed48 helpers、冻结数据或召回实现。根代理已实际完成第一次准备及两批共 12 次模型调用，独立评阅后又据此修复了当前任务来源写入边界；各次实验分别保存，不回写旧结果。

第一批固定六个原问题：germination_q06、paper_bridge_q02、fractions_q16、reading_q09、loops_q07、evaporation_q06。原五阳性、一阴性及最小完整证据集合不变。第二批是六个独立边界：父链、旁支、历史计划未重验、后代报告完成、忘记断链、忘记后新编号选择。第二批不并入原 96 题成绩。

每例重新调用冻结 `typed48_common::load/project`，在独立临时目录产生完整 60 条来源和 48 条真实类型记忆。按实际事件 session_id 核对各主题的十条来源，全部原活动结束后，统一用 s10 目标原文和 s07 已接受计划原文、实际来源 ID/version 重建历史快状态。这只是有来源的状态夹具，不证明模型曾正确提取目标和计划。选择活动调用真实续做预验、代次推进及创建事务，新 session/activity 和空快状态保持生产语义。

当前问题使用真实 learner_statement、shown 用户 turn 和 latest_input，再调用生产上下文。教学语言保持 helper 的实际 `zh` 设置；原问题中的中英混读和跨语标签指问题与来源关系，不要求所有问题输出英文。本轮不是完整双语输出验收。

这是生产活动隔离后的词面召回诊断，不是 BGE 排名复测。不会注入旧 top5、补塞漏召回来源或改写最终 context。原 helper 的 fixture_source_id 保留在临时数据库；若它或完整来源/问题别名、评测字段泄漏到实际请求，准备直接失败。自然词 reading、真实来源中包含答案的句子不属于标签泄漏。

独立 `review-cases.json` 保存真实来源映射、完整 live 来源、实际会话、边界操作及原标签。可见证据统计覆盖 context 全部 evidence，包括 continuation 目标/计划/完成来源，不能只看 slow_memory。引用合法和完整集合覆盖只是结构检查，不代表语义支持。实际上下文缺少原最小完整证据集时仍保持阳性标签，单列检索/上下文不足。

准备产物有完整实际请求、独立审阅输入和 manifest。manifest 绑定方法字节、probe_source_sha256、actual_executable_sha256/current_exe、冻结 helper、fixture、生产 Store/context/记忆策略/provider 相关源码、教学 policy/schema 与请求/审阅文件。运行前逐项核对；发生变化必须新建实验目录，不回写旧结果。Kimi 审计仅已核验 CLI 版本、二进制、agent 与空工具配置，不声称独立冻结模型 ID 或采样参数。

根代理运行命令：

```powershell
target/release/tutor-store-context-probe.exe --root . --output runtime/goal-baselines/20261104/tutor-store-context-first
target/release/tutor-store-context-probe.exe --root . --prepared runtime/goal-baselines/20261104/tutor-store-context-first --prepared-manifest-sha256 <实际manifest SHA256> --real-synthetic --batch 1
target/release/tutor-store-context-probe.exe --root . --prepared runtime/goal-baselines/20261104/tutor-store-context-first --prepared-manifest-sha256 <同一manifest SHA256> --real-synthetic --batch 2
```

默认准备不会构造、探测或调用 provider。显式远程使用现有生产 `probe → generate → close`，每例调用前 create_new attempt，拒绝隐式重复；错误保留并停止本批。不会投递答复、写入模型候选、播放音频或接触真实用户数据库。完整合成 validated reply 留给逐题审阅。

新增五项无模型合同回归：固定次序与五阳一阴；泄漏拦截且不误伤 reading；十二例真实 Store 上下文、来源范围和时间边界；continuation 证据计入引用覆盖但隐藏来源不能计分；attempt 不覆盖。已包含于根代理 workspace39/40 通过记录。结构测试、退出码 0 和正确引用不能替代语义审阅；即使十二例通过，也只支持下一阶段受控诊断，不支持默认开启语义召回或声称旧 36 题已修复。

## 首次实测结果

`runtime/goal-baselines/20261104/tutor-store-context-first` 使用冻结实测 exe SHA `ed3cc3d17529659c9ff471a6f08440910f176dcd4df82fa28c12cbc286812637`、准备 manifest SHA `f927135eecb2948f3d5bb7f1036747f63cddfd5a4230bb0bfcb5865efe382b4c`。12 次均结构有效、无重试、未投递/应用候选；两批 provider 正常关闭且本地 running 为 0，不推断远端取消状态。

独立审阅者已知方法，不是盲审或真人评审。原五阳性完整回答 3/5，阴性 1/1；植物与蒸发题缺关键来源，植物题明确说明信息不足，蒸发题仍未回答具体读法且没有明确指出证据缺口。保留原阳性标签，不能把合理拒答当作召回成功。六个边界正文 6/6，但连同候选只有 5/6：`descendant_completed` 提出两个仅引用历史 B 完成来源的当前 C task 更新。其 B 来源慢记忆摘要有依据，不一并视为伪造；原实验没有应用这三项候选。

实际 Store 回归先复现该写入缺口，再修复，详见 `HISTORICAL_TASK_GUARD_20261104.md`。后续新 policy 与新 guard 的六边界实验放在新目录，原始 12 例和此前 36 例判断不变。

## 新提示与写入边界后的定向复查

`tutor-store-context-current-source-policy-first` 另行准备 12 个实际 Store 请求，只执行原第二批的六个边界，不重跑或重新计分第一批六题。新 exe SHA `7f74cbfb955389cea90cdfcfdde71288cc0b2c092111225221030964a6eed374`、policy SHA `3d285fdf797183c9c0e718e4405007777c2a8c7f7953c6efab715547a847035b`、manifest SHA `d137fe3377031c206abd6af6fe26f9b964827179ab3e3e4e0a8da0741c737bef`。六次调用无重试，83.21 秒完成、provider 正常关闭；仍未应用候选。

独立审阅正文及含候选 **5/6**。`descendant_completed` 本次正确描述旧报告且没有提出当前更新；六例两类候选均为空。`sibling_branch` 仍失败：模型把可见的共同祖先 A（7/10）称为另一条续做分支的更正记录。旁支 B 的 3/5 并未进入上下文，属于历史归属语义错误，不能误报数据库泄漏，也不能因为候选为空就判整批通过。两个旧漏召回仍未解决。

本次使用新建 Store、来源身份和真实模型单次采样，并非严格因果 A/B；不能据此承诺提示彻底消除该类错误。程序写入边界由独立实际 Store 回归验证，模型正文关系判断仍需改进。下一步先明确生产上下文的当前/直接父项/更早祖先关系，再推进受控语义检索。

只读离线预检 `scoped-ranking-precheck/interpretation.md` 使用原冻结 BGE 向量：活动内 pure dense 的五个阳性最小证据集合均能覆盖；现 RRF 在植物题第 5/6 名同分，不能据此承诺稳定修复。阴性仍会返回候选。这不是在线 BGE、真实 Store 向量路径或新模型评测；生产默认语义召回没有开启。
