# 技术核验

当前交付按用户最新要求为全Rust：使用Axum/Tokio、Yew/WASM、nokhwa/Windows Media Foundation、ort、edgefirst-tracker的ByteTrack、CPAL/WASAPI、whisper-rs/whisper.cpp、原生SAPI COM和rusqlite。已按Cargo.lock锁定并在本机编译、运行，技术依据含 [ort](https://docs.rs/ort/2.0.0-rc.10/ort/)、[whisper-rs](https://docs.rs/whisper-rs/0.16.0/whisper_rs/)、[CPAL](https://docs.rs/cpal/0.16.0/cpal/)、[Whisper ggml权重](https://huggingface.co/ggerganov/whisper.cpp)。原生版独立验收见docs/RUST_*。下表记录前期选型调查，不再代表生产运行语言或ASR引擎。

云台分层记录：早期Tiny3原生Pan/Tilt读接口可调用，但1°UVC手动测试出现未命令垂直漂移，FAIL保留为历史。后续官方Center2.0.15.186已提取运行，OSC只读、显式两int profile和Rust有限位置/actor门禁已实现；新的物理方向、跟随与停止尚未验收，保持off/BLOCKED。详见PTZ_REPORT，不能继续沿用“无接口”，也不能把新代码或无运动预检当物理PASS。

日期：2026-11-01。以下区分官方能力、当前实现与实际测试；规格包中的旧资料不能作为本机PASS。

| 来源 | 本次用途与限制 |
|---|---|
| [Kimi command](https://www.kimi.com/code/docs/en/kimi-code-cli/reference/kimi-command) / [agents](https://www.kimi.com/code/docs/en/kimi-code-cli/customization/agents) | 对照本机2.3.1 help与真实事件，使用`--agent-file`、`tools: []`、`subagents: []`，不混用旧print参数。详细安全核验在KIMI_REPORT。|
| [OBSBOT SDK](https://www.obsbot.com/sdk) / [官方Center OSC](https://www.obsbot.com/explore/obsbot-center/osc) | SDK申请未代办；Center2.0.15.186以官方签名包提取运行，UDP16284已核验。官方表与UDP示例分别支持三int文档格式及Connected(1)显式注册；本机两int格式由只读交叉核验明确配置，不能猜Set方向。|
| [YOLOv8 Face作者](https://github.com/lindevs/yolov8-face) | 第三方face专用ONNX，SHA与元数据核验。仓库MIT与模型元数据AGPL不同，分别记录，不能将模型统称MIT。|
| [Ultralytics tracking](https://docs.ultralytics.com/modes/track/) / [Supervision ByteTrack](https://supervision.roboflow.com/trackers/) | 显式ByteTrack参数，ReID关闭。实际实现采用Supervision与ONNX，无PyTorch依赖。|
| [faster-whisper](https://github.com/SYSTRAN/faster-whisper) / [作者base权重](https://huggingface.co/Systran/faster-whisper-base) | 采用同一Whisper家族本地CPU int8；不是云ASR。HF本机超时，镜像只作传输；固定上游revision及SHA验证，见ASR_MODEL_REPORT。|
| [sounddevice0.5.1](https://python-sounddevice.readthedocs.io/en/0.5.1/api/streams.html) | 本机InputStream有界20秒内存缓冲，48k到16k重采样。|
| [Windows System.Speech](https://learn.microsoft.com/en-us/dotnet/api/system.speech.synthesis.speechsynthesizer) | 调用已安装离线声音；固定脚本、文本仅stdin JSON，不执行文字内容。真实播报与中断进程测试完成。|

只使用必要匿名教学上下文；摄像头、音频与推理本地，Kimi教学模型远程。没有实测费用节省比例，未提供token用量时为null。

## 官方OSC后续核验（2026-11-01）

签名安装包证据`runtime/vendor/setup-evidence.json`：版本2.0.15.186、Valid、REMO TECH Co., Ltd.，SHA-256 `fe0a97e9ebfc2d09dfc20e1f6a1cde99a46ad3c301da781c4df3f9d466f40617`；仅提取应用到`runtime/vendor/center/app/bin/OBSBOT_Main.exe`，未执行驱动/固件安装。Center实际UDP监听0.0.0.0:16284，应用传输仅限127.0.0.1并检查来源端口。

真实握手读到唯一槽0、名称`Tiny 3`、selected0、running=true、type18。显式`Tiny3Center2015PanPitch`将两int `[17,-24]`解释为pan/yaw17和pitch-24；同步原生Pan17/Tilt+24说明符号不同。默认官方roll/pitch/yaw三int解析不放宽。证据为`runtime/osc-read-rust-profile.json`及两份correlate记录；不构成Set或停止方向验证。

`runtime/ptz-osc-config.json`保留用户确认“两个总开关均已关闭”（user_confirmation，12:35:10+08），并与实时unlocked读查询分开。Rust门禁、末端guard、owner/停止代次及持久停止故障锁已实现；真实方向、跟随、停止需独立验收。v1固定mask预检BLOCKED_NO_MOTION，动态两帧预检`runtime/ptz-dynamic-preflight.json`为PASS_NO_MOTION_ONLY，命令0、latch=false，不能升级为物理PASS。

阶段部署PID90324的SHA为`c368c4ef5231ccba1d69a5085da21918ad2ed7bc3ba91b7570eaf462d0d37579`；旧30分钟报告仍只对应`469960ec7c441cb69fc254215e126457b9d814547a7008fe95f9e434f80b990c`。不将新源码/阶段程序套用为旧实测构建。
