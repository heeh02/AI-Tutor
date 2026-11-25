# 实验性语音打断：软件接线与已知未通过项

本片完成了播报期间监测、同一采音流接管、服务端取消旧回复与 UI 接管回执的源码接线。
功能默认关闭，要求显式 AEC 策略和每次语音轮换的 UI 选择；未替换已安装程序或已验 Demo。
当前不具备启用资格：固定回声负例已经实际触发误打断。

后续有限离线诊断已实际完成，见 [BARGE_ONSET_DIAGNOSTIC_20261109.md](BARGE_ONSET_DIAGNOSTIC_20261109.md)。原第 38 帧失败精确复现；切换公开配置未修复，10 秒实际回声适应后的对照表现不同。本片状态仍为未通过，不启用。

## 已实现的软件路径

- 前端接到当前回复的提交回执后，用心跳确认精确 request/generation/audio epoch，服务端才打开监测。
- 原采音 owner 保持有界滚动原始缓冲与绝对游标。检测候选保留最多 300 ms 起音前缀，不立即停止播放。
- 服务端核对当前回复及录音 token 后解除旧回复绑定，再由采音 owner 精确更新播放代次、保留原输入流和 AEC，开始识别。
- 接管只增加一个会话 generation，产生显式 adoption 回执；普通心跳中的代次增加不能授权 UI 开始录音。
- 监听有 3 秒续租、120 秒硬上限，候选有 2 秒接管期限；正常回复完成先关闭监听再重新就绪。
- claim 后失败统一同步撤销旧 speech gate、请求 watch 和 pending，再暂停匹配 token，防止任务自身 abort 留下 Starting 租约。

## 本次实际验证

| 检查 | 结果 | 证据 |
| --- | --- | --- |
| tutor-ui 测试首轮 | 135 通过 / 1 失败；提交回执没有推进已知 revision | `runtime/goal-baselines/20261104/destiny-handoff-ui-target.log` |
| UI 修复后 | 136 通过 / 0 失败 | `runtime/goal-baselines/20261104/destiny-handoff-ui-fixed.log` |
| ai-tutor 库测试 | 639 通过 / 0 失败 / 22 忽略 | `runtime/goal-baselines/20261104/destiny-handoff-native-first.log` |
| 固定 WAV、真实 Sonora AEC + VAD | 近端人声检测通过；纯回声负例失败，在第 38 个 10 ms 帧出现候选 | `runtime/goal-baselines/20261104/destiny-handoff-barge-fixed-audio.log` |

UI 修复记录提交回执的 revision，避免迟到的较旧 adoption 被当成当前回执；已有暂停与精确来源校验继续保留。
库测试覆盖滚动超过 20 秒仍不消耗接管后的话语预算、无效参考拒绝、开流 future 丢弃、正常结束与候选竞争、失败收尾及新录音不受旧失败影响。

固定 WAV 检查读取此前作者编写的成人合成语料，不打开麦克风或扬声器，不启动模型服务，不调用云端。
近端检测通过不抵消回声负例失败。AEC 成功执行和参考信号有效不能证明残余回声已经消除。
没有为了通过测试加入任意残留能量阈值、启动宽限或修改负例；失败原件保留，声学双讲和真实房间尚未验证。

## 构建环境记录

本次首先调用现有 PowerShell 构建辅助脚本，主机 Restricted 策略拒绝执行，未变更策略或使用 Bypass。
随后直接运行已安装 `cargo`。首次 UI 命令错误指定 `--lib`（该包只有 binary target），没有到达测试；更正 target 后得到了上表真实失败与修复结果。
这些环境/命令失败分别保留在 `destiny-handoff-ui-first.log` 和 `destiny-handoff-ui-direct.log`，不计作产品测试。

## 当前推进边界

用户当前优先级为 Demo 与 Destiny 结合。保留本片实验源码和负例证据，现有 Demo 继续使用已验语音版本。
Destiny 适配首先使用独立 Rust 语音桥的显式识别、朗读、取消；该桥没有自动监听/语音打断命令。
完整 P1 声学打断要求仍未完成，本次软件检查不替代设备验收。
