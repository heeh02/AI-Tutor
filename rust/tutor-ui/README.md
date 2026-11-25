# Rust WebAssembly 教师工作台

工作台的界面组件、状态轮询、HTTP 请求、目标选择、教学交互、录音操作代次、显示确认和记忆操作全部由 Rust 实现，使用 Yew 0.21 编译为 WebAssembly。

`frontend/index.html`、`frontend/style.css` 是浏览器的静态展示资源。`frontend/bootstrap.js` 仅导入并初始化 wasm-bindgen 生成的模块；应用业务逻辑没有 JavaScript／TypeScript 实现。wasm-bindgen 自动生成的 JavaScript 负责浏览器与 WebAssembly 的标准绑定。

构建：

```powershell
rustup target add wasm32-unknown-unknown
powershell -NoProfile -ExecutionPolicy Bypass -File frontend/build.ps1
```

构建结果写入 `frontend/dist`，由本机 Rust 服务通过同源认证接口提供。独立构建目录 `target/ui` 避免与本机设备服务的长时间编译互相等待。依赖解析受根目录 `Cargo.lock` 约束。

构建工具固定 wasm-bindgen CLI 0.2.100，来源为作者 [官方发布页](https://github.com/wasm-bindgen/wasm-bindgen/releases/tag/0.2.100)，下载资产 `wasm-bindgen-0.2.100-x86_64-pc-windows-msvc.tar.gz`。本地路径：`runtime/tools/wasm-bindgen/wasm-bindgen-0.2.100-x86_64-pc-windows-msvc/wasm-bindgen.exe`。下载压缩包 SHA-256：`54a3fb947464388a468ade86d65ffa334d6d2c74b7982723b34ecf6ec8c213d8`。该摘要记录实际获取的资产；没有声称作者发布了独立签名摘要。

服务的 Content Security Policy 需要允许同源脚本及 `wasm-unsafe-eval`，以允许浏览器实例化 WebAssembly；无需允许任意 JavaScript eval 或跨域资源。

保留的行为：

- 三栏真实工作台、教师控制、摄像头原帧与检测框同步、镜像坐标转换、临时目标与档案分离。
- 状态每秒更新；预览默认使用同源认证的二进制 WebSocket，每连接最多一个未确认画面。图像 decode 后提交同帧叠层，并在两次 requestAnimationFrame 间提供绘制机会后确认。HTTP/base64 仅作为用户明确选择的兼容通道，两路不并行消费。
- 真实后端对话；在 Yew `rendered` 生命周期确认显示后提交 `shown`，成功后才可自动播报。无固定对话或演示后端。
- 按住说话、快速松开处理、20秒上限、转写修改、停止／切换档案丢弃旧录音、过期异步结果保护。
- 录音开始回执提供代次编号，结束和迟到回执清理均绑定该次录音与会话，避免旧请求停止新录音。
- 保存、切档案、发送与设置更新期间阻止重复操作，等待最新状态取回后再开放；停止控制始终可用。迟到发送失败不会覆盖新编辑的问题；记忆列表只接受最新请求的回执。
- 模块人工来源、独立模拟档案、记忆召回／纠正／删除、活动恢复、真实诊断指标。

测试：

```powershell
cargo check --locked -p tutor-ui --target wasm32-unknown-unknown
cargo test --locked -p tutor-ui --target-dir target/ui
```

已执行通过17项原生 Rust 逻辑测试，覆盖模块回填、报告-only、草稿失败保留、结束前保存顺序、迟到结果隔离、模型失败后取回问题，以及录音代次、镜像坐标、来源和临时标签。WASM release打包通过。没有把这些结果记为真实浏览器、麦克风或扬声器验收通过。完整软件与真实模型流程见 `docs/PRODUCT_WORKFLOW_REPORT.md`。

2026-11-03新增回归场景：发送失败保留新草稿、空闲视频轮询降频、迟到记忆列表隔离、忙碌时停止入口仍可用。上述17项为历史验收数量；本轮最终测试与打包结果以项目优化报告为准。

后续 P0/P4 基础改动将帧请求与预览移入独立 `CameraPreview` 组件，避免每帧更新整个工作台；对话按最后一条 turn_id 跟随，支持固定24条窗口推进、上翻阅读和新回复入口。界面语言独立于档案教学语言，教师可分别选择提示方式、阅读表达与朗读语速；缺少新字段的旧状态使用兼容默认值。关键操作已有中英文案，用户/模型文本及原始诊断不翻译。详情和测试边界见 [UI基础记录](../../docs/GOAL_UI_FOUNDATION_20261103.md)，27组双语验收脚本在 `fixtures/ui-foundation-tasks-20261103.json`，现场执行状态为 NOT_RUN。

脸框和选中目标摘要使用后端 `label`，`track_key` 仅用于关联与接口提交。摄像头面板保留简洁的关闭／未标定状态，详细云台诊断与控制默认折叠。教学与语音不依赖云台。没有新增可绕过锁定的启用按钮。助手通过 `bin/tutorctl.exe` 控制已有服务，界面交给使用者操作。

浏览器与设备的成人现场复验应覆盖：真实预览与点框、文字生成与显示确认、自然语音录入和本地播报、播报时按键打断、切换档案时无旧语音或转写、记忆纠正删除和新活动确认当前搭建。

教师设置区分档案提示方式与本次活动偏好。会话偏好原话保持原文，可用“恢复档案提示方式”清除本次覆盖而不修改其他偏好。

P2 二进制预览将协议/待确认代次逻辑放在 `preview_stream.rs`，浏览器资源生命周期放在 `preview_browser.rs`，组件保留展示和用户交互。隐藏、关闭、断线或无像素状态立即清图并回收 ObjectURL；400ms 以上画面不允许选人/启用跟随，1000ms 清图。实现和验证边界见 [P2客户端记录](../../docs/GOAL_UI_STREAM_20261104.md)。

P1 临时录音字幕由独立 `LiveTranscript` 组件按200ms单请求读取本地音频状态，仅接受当前录音的新revision。字幕不写入输入框或教学消息；松开后的最终结果仍需检查、编辑再主动发送。范围和验证见 [字幕记录](../../docs/GOAL_UI_CAPTIONS_20261104.md)。

P4 记忆来源说明由 `MemoryEvidence` 按需读取，展示原句、日期、教育类型、帮助程度及重要性与可信度的不同含义。切档、更正、删除或收起会丢弃旧详情与迟到回执，不新增默认升格入口。实现、6项回归与合成验收脚本见 [记忆来源说明](../../docs/GOAL_UI_MEMORY_EVIDENCE_20261104.md)。

P4 长对话由独立 `ConversationHistory` 以最多50条/页只读浏览，游标回退栈最多64项；当前输入不参与历史状态。历史浏览时暂停隐藏回复的显示确认与自动朗读，新消息通过主动返回入口呈现。接口、投递边界和未执行的现场任务见 [长对话历史记录](../../docs/GOAL_UI_HISTORY_20261104.md)。

P1 可选文字流式由 `TutorStream` 独立轮询有界完整快照；`tutor_stream_state.rs` 管理不可改写的顺序前缀。默认仍是完整回复，仅服务端显式配置启用后接入。每次最多一个未确认分句进入 DOM；当前页面可见且非历史时，在 Yew 提交 DOM 后的一次 rAF 机会提交显示确认。它不证明人已阅读。最终升级原气泡；失败保留已显示前缀并标未完成。流式轮次跳过原有整篇 `shown` 与自动播报链，已完成轮次仍可主动朗读。实现、回归及未执行的浏览器任务见 [文字流式客户端记录](../../docs/GOAL_UI_TUTOR_STREAM_20261104.md)。

P4 活动页的 `ExperimentComparison` 提供独立 A/B 比较表与最近 10 组回看。状态明确选择、次数可未知、计划接受默认关闭；报告与待测项分开保存。`comparison_state.rs` 管理提交快照、同内容幂等重试、迟到回执与导航保护。未保存时先保存或清空再切活动，保存不自动提问或升格记忆。草稿只保留当前页面，刷新不保留；原始结果不翻译。实现、8 项待统一运行的逻辑回归与双语人工验收脚本见 [实验比较合同](../../docs/EXPERIMENT_COMPARISON_DESIGN_20261104.md)。

后续 P1 分句播报 UI 在服务器显式启用 `segment_speech:after_display` 后开放“下次发送自动朗读”。`speech_intent.rs` 用提交 nonce 和请求代次关联关闭意图；关闭、隐藏或查看历史后不会自动恢复本条。浏览器不创建音频队列，仅在关闭待确认期间暂缓显示 ACK。整篇自动播报仍跳过所有 stream turn。实现、11 项待统一验证回归及未执行的人工脚本见 [分句播报 UI](../../docs/GOAL_UI_SEGMENT_SPEECH_20261104.md)。

课堂本地化将活动摘要、模块报告、删除确认和操作反馈拆为小组件，统一报告来源标签，保留用户/模型原文；记忆来源说明按明确阅读设置提供摘要和可展开分项。清理了旧 HTTP 预览状态 helper 及 4 项过时测试，保留实际二进制链路与镜像坐标回归。当前范围和待统一验证边界见 [课堂文案记录](../../docs/GOAL_UI_CLASSROOM_LANGUAGE_20261104.md)。
