# Rust 原生 Kimi、教学与记忆验证

执行日期：2026-11-01，Asia/Shanghai。应用逻辑与以下验收程序均为 Rust。
Kimi 仍是本机已登录的官方 CLI，模型推理在远端；不发送摄像头图像或录音。

## 真实 CLI 验证

- 可执行文件：`C:\Users\RBMS\.kimi-code\bin\kimi.exe`，版本 **2.3.1**。
- SHA-256：`93878ac2bd12a8cf21b4e148e7f7705a7a6252c1398fd34eb50f8998ba0bf795`。
- 原生 Rust 验证记录：`runtime/kimi/verification-rust.json`。
- 真实中文模块解释：**PASS**，实际一次调用 9007ms。
- 强制读取合成 `secret-canary.txt`、引号/换行/`$(...)`/反引号攻击：**PASS**，
  合成秘密未出现在回复中，工具事件为 0；实际一次调用 8322ms。
- 真实 CLI 未返回用量：保留 `null`，没有伪造 token 数或费用。
- 独立原生核验命令：`ai-tutor.exe --verify-kimi --root <工作区>`。

## 无工具隔离与清理

`kimi.rs` 直接使用 Rust/Tokio 子进程参数数组，未使用 shell、Python 或 PowerShell。
运行时 agent 显式声明 `tools: []` 与 `subagents: []`；完整系统提示不引用继承模板。
每轮使用新的隔离工作目录和 `KIMI_CODE_HOME`，空 skills 目录与最小配置，
不恢复用户的编程会话，不加载 MCP、插件或 hooks。未经核验的版本、配置与扩展失败关闭。
原始全局 Kimi 配置不改写；认证失败只要求正常登录，不提取 token 或 API key。

凭证目录引用通过 Rust 调用 Windows `DeviceIoControl` 创建 NTFS junction。
凭证留在用户原有目录，应用不读取、复制或导出其内容。清理先移除 junction，再删除
经规范化路径检查确认属于本应用的临时目录；不会递归进入共享凭证目录。
成功、失败、取消和任务中止都清理本轮文本、会话、日志和临时 canary。
原生 Job Object 的 kill-on-close 负责关闭本次 CLI 进程树；遗留文件锁采用有界清理重试。
删除学习者数据前等待关联请求结束，并检查该匿名档案的应用自有临时目录。
异常退出后，下次启动清理已识别的死亡进程所属目录。

截止时间 90 秒；stdout 上限 256KiB、stderr 上限 128KiB；最多 4 个在途请求、
1 个有效执行槽。CLI 的步骤和尝试各限制为 1；没有隐形远程重试或额外摘要请求。
思考、诊断和 stderr 不显示或播报。仅接受已核验的 JSONL metadata 和 assistant 内容；
工具事件、未知事件、无效 UTF-8/JSON 与业务字段一律拒绝。

## 教学与记忆

`store.rs` 使用原生 SQLite，真实与模拟分别存储。记忆保留原事件、来源等级、
作用域、版本、supersedes 与删除 epoch。口述不升级为硬件确认；模拟不进入真实空间。
快状态只应用已显示回复的候选。生成后未显示的提示不能进入下轮已投递上下文。
播报中断保留为 speech_interrupted，保持“已显示过”的事实。

`context.rs` 保存完整本轮输入、目标、结果、待做步骤、模块报告与明确偏好；
选择完整的历史条目，来源去重并验证学习者归属。上下文最多 18000 字符，并检查
UTF-16 参数预算。当前输入只出现一次；历史摘要持续标明历史身份。超出必要事实预算
时明确失败，不截断本轮事实。新会话当前模块仍是未知。

`tutor.rs` 严格校验交付包全部业务字段与长度、枚举、来源 ID、request_id；
没有 shell、PTZ 或执行命令字段。历史来源支撑的任务字段标为 historical，
同时保存原始来源等级。模型不能直接写数据库。

自然语言更正必须引用旧受影响来源与当前明确更正来源；旧记录退出活动召回，
无关记录保留。无法关联的更正不持久化，应澄清目标。教师修改与删除采用事务，
清理派生对话、状态及 CLI 记录；旧 epoch 的整理不能写回。中文召回使用字符片段，
无关记录返回空，档案之间不串历史。

## 原生自动验证

已通过原生测试：JSONL 与工具事件拒绝、完整参数预算、NTFS junction 清理保留共享
目标、异步任务中止时子进程与目录释放、来源与作用域隔离、纠正与删除 epoch、
中文恢复、未投递内容排除、跨来源/无凭证 HTTP 与 WebSocket 拒绝、上传上限、
模型命令字段拒绝、显示确认、过期回复拒绝、选择视觉目标不改学习者。

API 测试使用独立临时 SQLite 合成记录，无真实模型或设备调用；不能替代硬件通过。
多轮真实模型产品流程另见 `RUST_E2E_REPORT.md`，模拟报告在 simulator 空间。

## 依据

- [Kimi CLI 参数与 JSONL](https://www.kimi.com/code/docs/en/kimi-code-cli/reference/kimi-command)
- [Agent 空工具列表与执行时约束](https://www.kimi.com/code/docs/en/kimi-code-cli/customization/agents)
- [Kimi 配置、hooks 与全局工具语义](https://www.kimi.com/code/docs/en/kimi-code-cli/configuration/config-files)
- [Kimi 数据位置与正常 OAuth 凭证](https://www.kimi.com/code/docs/en/kimi-code-cli/configuration/data-locations.html)
- [Windows reparse point](https://learn.microsoft.com/en-us/windows/win32/fileio/reparse-points)
- [Windows Job Object 配置](https://learn.microsoft.com/en-us/windows/win32/api/jobapi2/nf-jobapi2-setinformationjobobject)
- [Tokio 原生进程管理](https://docs.rs/tokio/latest/tokio/process/struct.Command.html)
