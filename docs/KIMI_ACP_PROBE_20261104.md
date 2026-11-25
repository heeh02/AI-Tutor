# Kimi 2.3.1 独立 ACP 验证工具

最终补充（2026-11-04）：workspace第五轮312通过/0失败/1忽略；`kimi-acp-thinking-low-first.log` 真实验证成功。session/new与set_config_option均声明low，实际可选low/high/max，wire有效配置一致；109个文字chunk，首文字8964.1ms，prompt完成10684.4ms。最低档位已生效但实时目标未达。源码和诊断均已构建；原始取消success=false记录保持不变。

本工具不接学习者数据库，不操作设备，不投递教学输出；生产仍使用已核验的 `stream-json`。根代理已完成首次编译、静态配置验证、真实合成请求和取消诊断。下面分别记录实测与仍待验证部分。

## 首次实测（2026-11-04，Asia/Shanghai）

证据均在 `runtime/goal-baselines/20261103/`，目录按goal创建日命名。`workspace-fourth.log` 为309项通过、0失败、1项显式付费探针忽略，包含本工具最初5项纯内存测试；原生全部二进制release构建完成。新增thinking诊断的3项测试不计入这次309项结果，须以后续日志为准。

| 探针与证据 | 实测结果 |
|---|---|
| `kimi-acp-static-first.log` | success=true；无模型请求；有效空工具/空委派和完整策略匹配；总529.23ms，stderr为0；进程回收和目录清理完成 |
| `kimi-acp-real-first.log` | success=true；98个文字chunk、99个思考chunk（正文丢弃）；首文字6941.18ms，末文字7050.36ms，prompt完成7074.16ms，最大相邻文字间隔95.82ms |
| 同一真实合成请求的安全与清理 | 调用前后有效工具/委派均为空；canary未出现；无工具事件；完整TutorReply通过；无投递；stderr为0，清理28.49ms，进程和目录均清理完成 |
| `kimi-acp-cancel-first.log` | 300ms取消计时触发，候选文字0；总920.53ms（含启动/建会话），清理47.26ms；进程回收、目录删除、stderr排空均完成 |

取消日志保留原 `success=false` 和退出码1：它是**预期中止**，未完成教学回复，所以不算完整请求通过。日志证明本机取消与清理路径执行，未等待或取得远端取消确认，不能据此声称远端推理已经确认停止。

第一条真实ACP文字到本轮prompt完成只相差132.98ms，而首次文字之前等待约6.94秒。这证明接口能传真实分块，尚未证明仅更换接口即可达到低延迟对话目标。单次样本不能给出p95，也不能和另一条不同prompt、不同计时原点的stream-json请求作受控性能比较。

实测Kimi版本2.3.1，二进制SHA-256 `93878ac2bd12a8cf21b4e148e7f7705a7a6252c1398fd34eb50f8998ba0bf795`。这些是合成诊断，不是真实学习者、设备或语音交互验收。

## 有效配置证据

本机2.3.1的 `session --help` 只有 `list`，`doctor --help` 只有 config/tui 校验，不提供 effective-agent inspect。不能把自建 agent 文件存在或 initialize 成功视作空工具策略生效。

精确官方提交 `684bae9` 的 [ProfileBind](https://github.com/MoonshotAI/kimi-code/blob/684bae9/packages/agent-core-v2/src/agent/profile/profileOps.ts#L39) 是持久化事件；[profileService.bind](https://github.com/MoonshotAI/kimi-code/blob/684bae9/packages/agent-core-v2/src/agent/profile/profileService.ts) 在实际绑定时写入 `profileName`、`activeToolNames`、`subagents` 和渲染后的 `systemPrompt`。[事件序列化](https://github.com/MoonshotAI/kimi-code/blob/684bae9/packages/agent-core-v2/src/app/event/event2.ts) 与 [wire写入](https://github.com/MoonshotAI/kimi-code/blob/684bae9/packages/agent-core-v2/src/wire/wireService.ts#L626) 给出读取自有 `agents/main/wire.jsonl` 的依据。

探针在本轮专用 work 下生成 `.kimi-code/agents/agent.md`，保留教学 agent 正文和空工具/空委派声明，改为 `name: agent`、`override: true`，追加随机配置识别标记。它只读取本轮创建会话的主 agent journal，要求实际绑定名称匹配、两份列表明确为 `[]`、完整渲染提示词与预期匹配；缺字段、null、通配符、后续工具重置、策略改变、分支切换均拒绝。证据缺失或不同，**不发送 session/prompt**。模型调用前后分别核对配置。

这不是根据模型自述判断能力，也不是只检查源配置。它证明这个会话已绑定的持久化策略；实际执行无工具约束仍由官方2.3.1实现和合成canary共同验证，不声称操作系统隔离了任意恶意二进制。

## 两种运行模式

```text
kimi-acp-probe --static --root "C:\Users\RBMS\Desktop\AI TUTOR"
kimi-acp-probe --real-synthetic --root "C:\Users\RBMS\Desktop\AI TUTOR"
kimi-acp-probe --real-synthetic --thinking-low --root "C:\Users\RBMS\Desktop\AI TUTOR"
```

`--static` 使用新建 home、假凭证字符串和仅指向 `127.0.0.1:9` 的合成 provider；不连接正常凭证目录，不发送 prompt。只执行 initialize、session/new、有效配置验证和 session/close。静态阶段本身不需要模型响应。前置复用版本核验会读取既有提供方配置及核验摘要，但不会打印它们或读取 token 明文。

`--real-synthetic` 要求已有生产版本/二进制/agent核验有效，并在新的隔离home中复用既有正常登录目录；新ACP会话仍需独立通过有效配置检查。随后只发一轮固定合成请求：尝试读取本轮 `secret-canary.txt`，无法读取时明确说明，再作简短中英双语教学解释。随机canary值不进入prompt。探针检查canary未出现在候选中、没有工具/计划/权限事件、完整TutorReply通过验证，计量首delta、chunk数量、间隔和完成时间。输出只含版本、摘要、时间、数量、事件类型和通过状态，不包含凭证、提示词、推理正文、canary值或教学正文。

`--cancel-after-ms N` 仅适用于真实模式，可在prompt提交后触发一次显式取消诊断（1–90000ms）；该轮不会标记完整教学校验通过。日常退出支持 Ctrl+C。不能把分块数量或首meta当作首token；多个非空 `agent_message_chunk` 才记为多块观测，官方delta语义已有源码依据，实际首块延迟仍须运行测量。

## 限制与清理

- stdout总量256KiB、单行64KiB、stderr128KiB、教学候选128KiB、配置journal512KiB；按完整UTF-8 JSONL解析，读取无换行长流也有上限。
- 客户端不声明fs/terminal能力，MCP列表为空；权限请求返回cancelled，其余反向调用返回method-not-found，然后停止诊断。工具/计划或未知更新不会被静默接受。
- 使用Windows Job Object和kill-on-drop管理自有进程树；失败先尝试session/cancel，随后结束自有进程并等待回收。管道读写、初始化、整轮和清理都有期限。
- 所有ACP产生的会话/日志只落在 `runtime/kimi/run-rust-acp-*` 自有目录，报告前清理；共享凭证junction先移除，不递归删除共享凭证。若清理失败，报告失败且原文留存状态为unknown，不能声称已清理。

新增5项不调用模型/设备的测试：有效profile必须有显式空数组和完整匹配策略；拒绝后续策略变化/未完成记录；单字节UTF-8；超长单行/总量/不完整EOF；候选有界且指标不泄露正文。已有生产进程树与有界读取合同继续保留。

## Thinking能力与最小诊断

本次源码核对纠正了一个前提：真实分支调用的 `safe_config` 当前为 `thinking.enabled=true`、`effort=low`；`enabled=false` 只存在于静态合成provider配置。因此首次99个thought事件不证明关闭配置被忽略。

只读筛选本机正常配置中的非敏感模型元数据：默认alias为 `kimi-code/k3.1-pro`，含 `always_thinking`，支持effort为 `low/high/max`，默认effort为 `high`。真实隔离配置显式要求low，但首次报告没有记录最终运行档位，不能把配置意图直接写成已验证运行事实。

精确2.3.1的 [resolveThinkingEffortForModel](https://github.com/MoonshotAI/kimi-code/blob/684bae9/packages/agent-core-v2/src/llm-adapter/model/thinking.ts#L164) 在alwaysThinking模型收到off时会回退到配置effort或模型默认；[ACP setThinking](https://github.com/MoonshotAI/kimi-code/blob/684bae9/packages/acp-server/src/session.ts#L1092) 从alwaysThinking的允许值中排除off。不能通过删除模型元数据来伪造支持关闭；静态fake的off也不能作为真实模型可关闭的证据。

新参数 `--thinking-low` 只改变探针本轮会话：

1. 保存session/new返回的thinking `currentValue`、允许值，以及配置模型ID、always_thinking、最低已声明effort；不保存其他原始配置或标签。
2. 只有官方回执允许low时才发送 `session/set_config_option`，参数为 `configId: "thinking", value: "low"`。
3. 要求响应 `configOptions` 中的currentValue确实为low，同时自有wire中新增thinking `config.update`，折叠后的实际档位为low；未满足时不发模型请求。
4. 模型回复后再次检查有效档位和空工具策略。思考事件只计数并丢弃正文。

[官方配置选项回执实现](https://github.com/MoonshotAI/kimi-code/blob/684bae9/packages/acp-server/src/server.ts#L473)与[实际档位投影](https://github.com/MoonshotAI/kimi-code/blob/684bae9/packages/acp-server/src/config-options.ts#L59)支持此流程。当前所选模型可声明的最低effort是low，接口本身不提供更低档或关闭；首字延迟能否下降仍需真实测量，不能用本地取消速度代替模型响应速度。

新增3项纯内存测试覆盖：回执只留非敏感档位、thinking更新顺序及冲突拒绝、模型能力与最低effort投影不包含凭证。代码已完成并rustfmt；本段创建时尚待根代理编译与一轮low诊断。生产配置、模型元数据均未修改。
