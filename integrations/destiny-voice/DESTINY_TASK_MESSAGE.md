# 已获授权并发送的 Destiny 单次交接草稿

目标 chat：`01a1a5c6-e434-7702-a4b7-4a281ecbf8c1`（local，Destiny / SoulLink 项目接手任务）。
用户于 2026-11-10 明确授权“允许发送，推进离线接线（推荐）”，已向原 chat 发送一次。实际发送正文与成功回执保存在 `runtime/goal-baselines/20261110/destiny-handoff/`（相对 AI Tutor 根）。这次仅交接离线源码工作，不授予对原窗口或运行资源的接管权限。下方保留原草稿正文。

---

用户希望结合 Destiny 的伙伴体验与 AI Tutor 的本地双语语音。AI Tutor 已完成 C#5/.NET Framework 4.5+ 适配件，现请由 Destiny 既有源码负责人接续最小离线接线工作，保持单一 writer 与原有接口冻结流程。

可直接读取的交付：

- 源码与接入说明：`C:\Users\RBMS\Desktop\AI TUTOR\integrations\destiny-voice\README.md`。
- DLL 与六份源码：`C:\Users\RBMS\Desktop\AI TUTOR\runtime\goal-releases\20261109-destiny-voice-adapter`，DLL SHA256 `4ec45737c46568021e2d995f4157505d3ce048a7e14f41844ffd8cf6c0c566dc`。
- 接线边界和需要审定的 capability 后继已写在上述 README；属于待审设计，不冒充已冻结 Destiny 契约。
- 只读来源清单：同目录 `destiny-source-base.json`，记录当前 17 份相关文件的 SHA256，便于接续时核对。
- 无模型消费者检查 18/18：`runtime\goal-baselines\20261109\destiny-adapter-next\20261109T055735.396148Z-8c21c4a5\results.json`。
- 新 C# 消费者已实际完成固定中英文 WAV 各一次识别、Qwen PCM 各一次完整播放、一次开始后取消，以及两模型服务与 Rust helper 正常退出。证据及独立复核：`runtime\goal-baselines\20261109\destiny-adapter-real\20261109T055905Z-81ad3b20`（上述 runtime 相对 AI Tutor 根）。固定语料、0 云端调用、未开麦克风、未操作 Destiny 窗口。

实施范围：在当前合法源码和 Native/UI owner 下评审必要的 Voice capability 后继，接入原 DesktopBridge/原生按住录音/转写编辑/已保存回复朗读流程。使用已有 Rust bridge 承担本地 ASR/TTS，保留 Destiny 账号、伙伴、Chat、Memory 和普通 System.Speech 路径。统一 20 秒录音上限、显式准备状态、中英朗读语言、精确取消和异步关闭。先完成无模型原生/TS 消费者验证，不另造聊天控制器。

注意：这次交接仅请求新的离线源码工作。不得恢复旧 Esc 停止的窗口操作，不接管或清理原用户控制 profile，不恢复原真实 Flash/旧 R1/数据库/部署/发布，也不读取、传递或搜索任何凭据。现有真实模型和窗口验收的许可、费用及交接边界继续按原任务处理。AI Tutor 这边没有修改 Destiny 工作树；只读检查仍为 HEAD `53baaf67` 和原四份治理文档 WIP。

接线源码及无模型消费者结果完成后，提供一份集中结果和实际待验项。真实窗口/麦克风验证应在用户明确交接后进行；已验 C# 进程链不替代 Destiny UI 验收。
