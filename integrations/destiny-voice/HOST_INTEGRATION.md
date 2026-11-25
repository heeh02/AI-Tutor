# Destiny 接线提案：原生启用 Qwen，沿用聊天与语音操作

状态：**PROPOSED，未实施、未冻结**。只读核对 Destiny `/home/owner/workspaces/destiny`，HEAD `53baaf67`；依据根 AGENTS、STATUS、PROPOSAL、current plan 与已冻结的 `2026-11-06-companion-voice-desktop-v1.md`。
本提案交 `core_delivery`（现有 Native/UI 唯一 writer、原 Contract owner）审定；本文件不授予 Destiny 写入或运行权。184952 窗口的实体 Esc 交接限制仍有效。

## 选择与最小出口

| 方案 | 新增公开面 | 结论 |
| --- | --- | --- |
| 网页按钮准备、网页选择语言 | prepare/cancelPrepare 命令、准备操作身份/事件、play.language、capability 后继 | 能做，但 Demo 首轮改动更多 |
| 原生工具栏准备、原生选择语言 | capability 后继及其更新事件；原有 play/stop/input 命令不变 | **选择此方案**，与现有 native-hold 入口一致 |

原生新增“启用本地中英语音”按钮和“朗读语言：中文 / English”选择。首次显式启用才启动 helper、绑定、预热；导航、hello、bind 和载入聊天均不加载模型。页面显示准备状态并继续允许文字聊天。
出口为：启用→等待准备完成→原生按住录音→可编辑识别稿→明确加入输入框→原有发送→手动朗读当前已保存回复。没有后台监听、自动发送、自动播放或第二个聊天 FSM。

## 精确文件职责（均为下一实施波次）

| Destiny 路径 | 最小改动 |
| --- | --- |
| `apps/desktop-windows/Program.cs` | 将已核验 helper EXE 与显式本地模型 root 作为 `VoiceBridgeOptions` 注入窗口；保留单实例 mutex、原 profile/reply-mode/测试入口，不读取凭据或扫描其他应用。路径只留原生层。 |
| `apps/desktop-windows/CompanionWindow.cs` | 增加原生准备/语言控件、准备耗时与失败提示；hold 受 readiness/binding/busy 控制；把同步 FormClosing 改为一次性异步关闭。 |
| `apps/desktop-windows/DesktopBridge.cs` | 持有唯一 Qwen session；保留来源/nonce/sequence/scope 检查；选择原 System.Speech 或 Qwen 路径，投影现有 playback/input 事件及新增 capability 事件。 |
| `apps/desktop-windows/LocalQwenVoice.cs`（拟新增私有类） | 封装六文件 client、helper 操作与宿主操作身份映射、原生 hold、取消终态与关闭；不新增公开业务 port。 |
| `apps/desktop-windows/WindowsAudioCapture.cs` | PCM16/16kHz/mono 不变；容量 `960000→640000`，即 20 秒。保留 reset→unprepare→close→释放内存次序。 |
| `apps/desktop-windows/WindowsSpeechInput.cs` | 普通模式录音 deadline `30000→20000`；保留原识别器/10 秒识别界限。Qwen 路径直接消费关闭设备后的 WAV，不经过 System.Speech 识别。 |
| `apps/desktop-windows/WindowsSpeechOutput.cs` | 保留普通模式；Qwen 不调用它播放。原 1200ms 停止计时器只适用 System.Speech。 |
| `apps/desktop-windows/BridgeMessages.cs` | 更新冻结后协议常量/严格校验；现有 play payload 不加语言、WAV 或路径。 |
| `src/modules/chat/ports/companionVoice.ts` | 声明下述 capability 后继；原 CompanionVoicePort 方法和 scope/operation 身份不变。 |
| `src/modules/chat/adapters/browser/desktopVoiceBridge.ts` | 同步严格 schema、初始 snapshot、新 capability 事件；保留 3000ms 命令 ACK 与原有 fencing。 |
| `src/modules/chat/ui/useCompanionVoiceControls.ts`、`CompanionVoiceControls.tsx` | 投影准备/语言、就绪前禁朗读、修改中文限定文案；window 控制和文字聊天不等待预热。保留编辑稿与显式 append/send。 |
| `src/modules/chat/adapters/browser/webSpeechOutput.ts` | 适配新 capability 形状，仍只声明已枚举本地中文合成；不增加浏览器 ASR。 |

六个适配件源码纳入宿主编译清单，命名空间 `SoulLink.Desktop.Voice` 不改；编译继续 C#5/.NET Framework 与 `System.Web.Extensions`。`companionVoice.ts` facade、composition 的 native 优先选择与 Auth invalidation 保持单一入口；调整相应 fixture/构建清单即可。

## 原 owner 需冻结的最小公共后继

现有 TS/Zod 与 C# 都严格拒绝额外字段；不能把扩展伪装成向旧消费者兼容。建议成套升级为 `soullink-desktop-v2`，旧 EXE/旧页面仅作为完整旧版本配对保留；混版失败关闭语音，不猜协议。

| 项目 | 拟冻结精确变化 |
| --- | --- |
| `VoiceCapabilities.output/input` | 各增加 `'windows-local-rust-qwen'`；其余既有枚举保留。实际不可用时仍为 `'unavailable'`。 |
| `VoiceCapabilities.preparation` | 必填 `'not-required' \| 'idle' \| 'preparing' \| 'ready' \| 'failed'`；普通 System.Speech/Web 用 not-required，Qwen 初始 idle，显式启动到预热终态期间 preparing。 |
| `VoiceCapabilities.locale` | 扩为 `'zh-CN' \| 'en-US' \| null`，明确表示**当前朗读语言**，不是 ASR 识别语种；Qwen 两语言自动识别。 |
| host `kind:'capabilities'` | payload 为完整 `VoiceCapabilities`，沿用 protocol/documentNonce/sequence，无 requestId；ready 初始事件也包含同一新形状。 |
| 其余 wire | 无新网页命令，无 prepare 请求 ACK，无 play.language；voiceName 原有上限不变，只显示实际已知名称或 null。 |

preparing/idle/failed 时 Qwen output/input/inputActivation 均 unavailable；只有模型预热成功、当前 helper 绑定成功且允许当前来源时才开放媒体。换绑定中同样关闭媒体入口，恢复不等于自动播放。
新 capability 事件是当前文档的宿主能力事实，严格核 nonce/sequence，不能重建已释放 scope。异步完成必须核对原生会话代次；旧进程/旧准备完成不能把 failed/已退出状态改回 ready。
不新增错误枚举：未就绪命令拒绝 UNAVAILABLE，互斥占用 BUSY，语言拒绝 UNSUPPORTED_LANGUAGE；媒体失败映射 INPUT_FAILED/OUTPUT_FAILED。准备失败通过 preparation=failed 显示可操作提示，不发送原始错误/路径。

## 原生映射与生命周期

1. **准备**：点击后先停止原 System.Speech 媒体，置 preparing；顺序 `StartAsync→BindAsync(opaqueViewId)→Prepare().Completion`。hello 只证明连接，只有 completed 才为 ready；初次超过 200 秒仍显示“正在准备”及真实耗时，无假百分比，不套 3 秒 ACK/旧 ASR 10 秒时限。失败不自动重试。
2. **语言**：原生下拉框默认中文，用户可选 English；宿主接受 play 时截取当前选择，映射 zh-CN→zh、en-US→en，随该操作保存。媒体在途禁改选择；不根据人格、键盘、ASR 结果或文本猜语言。页面只读显示所选朗读语言。
3. **录音**：原生 MouseDown/Space 才打开设备；最长 20 秒，时间到明确取消并丢弃，不把前 20 秒当完整录音。MouseUp 先完全关闭设备，再调用 `Transcribe(wav)`，清零宿主持有的 WAV；超限/空音频/超 2000 字识别结果拒绝，不截断、不写文件。
4. **身份**：宿主持有 `{documentNonce,完整 VoiceScope,operationId,assistantMessageId?}→VoiceOperation` 映射。Web binding 仍来自已验证 roster；helper owner 只用不透明 viewId，helper generation 与原有 scope generation 各自保留，绝不充当登录凭证。
5. **结果**：提交成功可 ACK/starting；只有 `SpeechStarted` 才 speaking，completed 才 finished/识别稿。回调先派发 UI，再检查当前来源、nonce、完整 scope/operation 及 `CanDeliver`。终态清理内部占位不能依赖页面仍存活；撤销后不交付文本。
6. **取消**：先同步 fence，再 `CancelAsync(exactOperation)`；其返回的 cancel_requested 不是停止完成，继续等待原 operation.Completion。Rust 停止可达 4 秒，不用旧 1200ms 超时；非空 StopError 必须呈现失败/未确认，不能显示“已停止”。
7. **互斥**：helper Busy 在终态前不放行新媒体。为保留旧 single-replace，play 替换先撤销旧操作、保存最多一个新显式意图，ACK 后异步等旧终态再启动；新的替换淘汰前一待启动意图。原生 hold 若还需等停止，要求用户停止完成后重新按住，不后台延迟开麦。
8. **原生命周期**：release/logout/导航/renderer loss/suspend 清 scope 并停止；最小化/隐藏取消媒体与未应用稿但保留绑定，恢复静默；普通失焦只取消录音。旧来源、旧取消、旧完成绝不影响新 scope。`InvalidateBindingAsync` 不取消无 scope 的 Prepare，宿主必须另外取消保存的 prepare operation。
9. **关闭**：FormClosing 首次设 `e.Cancel=true`、closing=true、禁用媒体并发布 window.closing；异步 await session.ShutdownAsync，记录 CleanupConfirmed/ExitObserved/ExitCode/ForcedKill。完成后置 closeAllowed 再 Close；重入只等待同一任务，不在 UI 线程调用同步 Dispose/Result/Wait。

关闭等待不得依赖现有 `Dispatch`（closing 时它会丢弃回调）；终态清理在 session 内完成，最后由捕获的 UI 上下文完成关窗。失败明确标记清理未确认；不能因强杀或超时声称服务已清理。当前 client 默认关闭界限 120 秒，仍是可响应的异步等待。

## 普通模式与采用依据

普通启动继续 System.Speech（现有中文能力、真实回调与原停止行为）；用户点击原生启用按钮才切 Qwen。Qwen 准备失败后保持不可用，明确重试或重启普通模式，不自动切换宿主模式。两模式均采用新 20 秒录音提示；独立 AI Tutor Demo 保留，开始真实 Qwen 验证前须明确唯一模型服务 owner，不能并起争用 5070。
已验 Rust bridge 允许首次输出前回退 SAPI。LocalQwenVoice 必须读取实际 `SpeechStarted.receipt.engine_kind`：若为系统回退，在原生语音状态栏明确显示“本条使用系统语音”，不得把配置的 Qwen capability 当作本条实际引擎；不另行在 C# 播放或重试。网页 capability 表示服务能力，实际引擎提示留在原生层，故不新增 playback wire 字段。
保留 [Win32 waveInReset](https://learn.microsoft.com/en-us/windows/win32/api/mmeapi/nf-mmeapi-waveinreset) 的返回缓冲区生命周期；采用 [WinForms 跨线程派发](https://learn.microsoft.com/en-us/dotnet/desktop/winforms/controls/how-to-make-thread-safe-calls)，Framework 用现有 BeginInvoke，不引入仅 .NET9 可用的 InvokeAsync；普通模式继续 [SpeakAsync](https://learn.microsoft.com/en-us/dotnet/api/system.speech.synthesis.speechsynthesizer.speakasync?view=netframework-4.8.1) 和真实完成事件。没有新框架或 NuGet 需要。

## 可执行验证入口与边界

以下为**后续执行入口，本次均未运行**；Destiny 实施与真实窗口验证须由原 owner 在合法交接后进行。

```text
# AI TUTOR：现有无模型适配件消费者
.venv\Scripts\python.exe integrations\destiny-voice\run-tests.py
# AI TUTOR：已有显式真实本地模型消费者，不开麦克风
.venv\Scripts\python.exe integrations\destiny-voice\probes\run-real.py --real-local-models --voice-root C:\Users\RBMS\.cache\ai-tutor-voice-demo-20261109
# Destiny：已有定向测试与类型入口，实施后扩展原用例
npm test -- tests/chat/voice
npm run typecheck:app
```

定向增加：用虚拟时钟模拟准备持续 201 秒时 ACK/window/文字操作仍可用；中英选择被实际 Speak 消费；20 秒取消与关闭设备后转写；旧 scope/nonce/prepare 完成丢弃；取消超过 1.2 秒但有真实终态；带 StopError 不报成功；首次输出前系统回退如实呈现；异步关窗与 System.Speech 普通模式。之后只做一条真实原生用户链和听感/麦克风检查。
编译沿原 owner 已审直接 csc/EXE 入口，不执行被 Restricted 拦截的 PS 脚本或更改策略。本次只新增本提案；未改 Destiny、未编译/运行、未操作窗口/进程/数据库/凭据，未把已有适配件测试称为 Destiny 集成验收。
