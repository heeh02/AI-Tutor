# Destiny 本地语音适配件

这六个 C# 文件将现有 Windows 宿主连接到 `ai-tutor-voice-bridge.exe`，提供本地中英识别、朗读、取消及服务退出。
这是可编译、可独立运行的接入组件；**尚未编入 Destiny，也未替换已验 AI Tutor Demo**。
使用 .NET Framework 4.5+、C# 5 和系统 `System.Web.Extensions`，无需 NuGet 或新的桌面框架。

## 模块

| 文件 | 职责 |
| --- | --- |
| `OwnedVoiceProcess.cs` | 无窗口子进程、UTF-8 JSONL、有序写入、持续读取、退出观察；stderr 只计数 |
| `VoiceBridgeClient.cs` | 请求关联、真实播放开始事件、当前绑定交付检查、终态结算 |
| `VoiceBridgeRequests.cs` | 预热、绑定、识别、朗读、精确取消与解绑 |
| `VoiceBridgeLifecycle.cs` | hello、可等待的退出、清理确认；超时强制退出不算清理成功 |
| `VoiceModels.cs` | 宿主接口、操作身份和结果；可注入 transport 供测试 |
| `VoiceWave.cs` | 内存 WAV 格式与 20 秒长度检查，不录音、不截断 |

Rust 桥负责实际模型服务及播放设备。宿主负责录音、UI、账号与聊天；语音操作本身不发送聊天、不写记忆。
没有监听端口，也不把 WAV、模型服务描述符或进程管道交给网页。

## 调用顺序

1. 创建 `VoiceBridgeClient(new VoiceBridgeOptions { ExecutablePath = ..., VoiceRoot = ... })`，调用 `StartAsync()`。
   hello 表示进程连接成功，尚未加载模型。
2. 用 `BindAsync(opaqueViewId)` 绑定当前经过 Destiny 验证的视图。每次绑定的 helper generation 自动递增。
3. 用户选择启用语音后调用 `Prepare()`，显示准备状态，等待其 `Completion` 的 `Event == "completed"`。
   模型初次加载可能需要数分钟；不要套用 Destiny 的 3 秒命令 ACK 时限。
4. `Transcribe(wavBytes)` 接受已关闭录音设备得到的 PCM16/16kHz/mono WAV，最多 20 秒。
   `Speak(text, "zh"|"en")` 支持最多 2000 个 UTF-16 单元，分段由 Rust 执行。
5. `SpeechStarted` 才表示观察到实际播放开始收据，`accepted` 不是开始或完成。
   回调在线程池/读取线程发生；派发到 UI 后再次用 `CanDeliver(operation)` 核对再更新视图或应用识别稿。
6. `CancelAsync(operation)` 立即撤销旧结果交付，再发送精确 ID/sequence 取消。
   `cancel_requested` 只是取消已受理；等待该操作 `Completion`，并保留非空 `StopError`。
   `Busy` 在操作终态前保持，避免新旧媒体重叠。
7. 换伙伴使用新绑定；退出登录/释放视图调用 `InvalidateBindingAsync()`。
   窗口关闭时等待 `ShutdownAsync()` 或 `DisposeAsync()`，核对 `CleanupConfirmed`、`ExitObserved`、`ExitCode`。
   WinForms 关闭事件应异步等待后再退出窗口，避免在 UI 线程同步阻塞。

绑定只是媒体关联，不是身份认证。调用者继续沿 Destiny 原有账号、伙伴、conversation/DNA revision 检查建立绑定。
同一 scope 内宿主仍须核对当前操作 ID；`CanDeliver` 不能替代原有草稿与视图状态管理。

## 接入 Destiny 的具体改动

完整的最小宿主后继见 [HOST_INTEGRATION.md](HOST_INTEGRATION.md)：选择原生工具栏预热和语言选择，网页仅增加 capability 状态与更新事件，保持现有 play payload。该文件是待 Destiny 原 owner 审定的提案，尚未实施或冻结。

当前只读核实的 Destiny 基线为 `53baaf67`，Native/UI 唯一源码 owner 为 `core_delivery`。
本目录不修改其工作树。接入时只需集中修改现有路径：

- 在 `apps/desktop-windows/DesktopBridge.cs` 持有一个 client，使用现有窗口退出和换绑定流程管理生命周期。
- 保留 `WindowsAudioCapture`；将 `WindowsSpeechInput.TranscribeClosedWave` 的内存 WAV 交给 `Transcribe`。
  原生录音上限从 30 秒调整到 20 秒，并同步界面提示，禁止静默裁掉末尾。
- 将现有 `WindowsSpeechOutput` 调用映射为 `Speak`/`CancelAsync`，使用 Rust 回执投影旧播放状态；C# 不重复播放。
- 为 `companionVoice.ts` 及 `desktopVoiceBridge.ts` 增加准确的本地 Rust/Qwen capability，扩展当前仅 `zh-CN` 的语言表达。
  明确朗读语言来源；ASR 自动识别语言。这里是待冻结的契约后继，不冒充已存在的 wire 字段。
- 保留“识别稿可编辑→明确加入输入框→发送”及当前保存回复的朗读入口。
  首版不增加后台监听、自动插话、自动发送或新的聊天状态机。

Destiny 的 1.2 秒 System.Speech 停止确认不能直接套在最长 4 秒的 Rust 停止收据上。
本组件会立即停止交付旧结果，并等待真实终态；软件停止确认不等于声学静音测量。
主机启动前应明确由哪个应用持有语音模型，避免同时运行两个独立实例抢占同一 5070。

## 验证

无模型消费者测试：

```text
.venv\Scripts\python.exe integrations\destiny-voice\run-tests.py
```

它直接调用已安装的 C# 编译器，不执行 PowerShell 脚本或变更执行策略。
用 fake transport 和自有合成子进程测试排序/取消/退出，再用冻结 Rust release 在新空 root 执行 hello、bind、status、未预热媒体拒绝和 shutdown；不发送 prepare。
每次创建新的证据目录，保留失败，不保存原始协议或 stderr 正文。

显式真实本地模型消费者（会加载 Qwen 服务并播放固定合成短句，不打开麦克风）：

```text
.venv\Scripts\python.exe integrations\destiny-voice\probes\run-real.py --real-local-models --voice-root C:\Users\RBMS\.cache\ai-tutor-voice-demo-20261109
```

该探针冻结 C# 源码、已验 Rust EXE 和固定 WAV 身份，检查中英识别、Qwen 实际播放开始/完成、长句取消及 owned 服务退出。
它不调用云模型，也不操作 Destiny 窗口，不能据此认定 Destiny 产品接入或自然麦克风体验完成。
每次结果以 `runtime/goal-baselines/20261109/destiny-adapter-{next,real}/` 的独立报告为准。

## 采用依据

使用系统 `Process` 重定向 stdin/stdout/stderr，保持两条输出流持续消费，并在进程退出后确认读取完成，采用
[Microsoft RedirectStandardOutput 文档](https://learn.microsoft.com/en-us/dotnet/api/system.diagnostics.processstartinfo.redirectstandardoutput?view=netframework-4.8.1)
说明的进程通信机制；按 [CreateNoWindow](https://learn.microsoft.com/en-us/dotnet/api/system.diagnostics.processstartinfo.createnowindow?view=netframework-4.8.1)
要求使用 `UseShellExecute=false`。没有引入外部进程框架。
