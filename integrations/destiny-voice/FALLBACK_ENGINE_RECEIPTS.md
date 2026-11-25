# 已交付 Rust helper 的系统语音回退标识

2026-11-10，只读核实。用于回答 Destiny `local-qwen-voice-20261110/design.md` 第 11 节的生产方词汇问题；不修改六份已审 C# 源码、DLL、模型或 Destiny 契约。

| 场景 | `receipt.engine_kind` | `receipt.milestone` |
| --- | --- | --- |
| 已配置 Qwen 的原生 PCM 输出开始 | `qwen_pcm` | `first_callback_submission` |
| 首次 PCM 输出前回退到 Windows SAPI | **`sapi`** | **`sapi_speak_accepted`** |

SAPI 完成/中断的 `SpeechOutcome.engine_kind` 同样是 `sapi`。
`sapi_speak_accepted` 表示原生 `ISpVoice::Speak` 异步调用已受理，不是首个 PCM 回调或实测扬声器起音。
普通 JSONL 请求的 `accepted` 仍只表示接收了操作；它与包含上述 receipt 的 `speech_started` 是两个事件。

生产方出处是冻结的 `audio/speech_output.rs` 第 310 行：

```rust
start_receipt(&mut request, "sapi", "sapi_speak_accepted");
```

第 223 行设置 SAPI 完成/中断结果；`voice_bridge/jobs.rs:72` 原样序列化 receipt，`voice_bridge/mod.rs:252` / `:264` 发出 `speech_started`，没有把引擎词汇改名。

冻结源路径、各 SHA256 和生产方行号见同目录 `FALLBACK_ENGINE_RECEIPTS.json`。
本次实际核对了 helper SHA `a810538cd71adc5ec32b2a95fed4904118a6c406621e6be99eb57b55d982028b`、release manifest → 合并 source manifest → parent manifest 的摘要链，以及四份冻结 Rust 源码的摘要。
这不是逐字重现构建或新的实际 SAPI 回退运行；本次未启动 helper、模型、声音或窗口。

不要从 C# fake transport 的任意样例值推导生产词汇。其 `windows_sapi` / `first_pcm_submitted` 样例仅用于验证适配件原样保留不透明 receipt，**不是**此冻结 Rust helper 的实际 token。宿主应使用上表生产标识，并保留未知引擎的未知状态，不将其显示为 Qwen。
