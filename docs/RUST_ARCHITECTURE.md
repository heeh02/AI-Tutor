# Rust 运行架构

用户最新要求：全面使用Rust，不进行语言性能比较。本项目应用逻辑已按此迁移；先前Python与React实现仅保留为历史参考，不能代替Rust版验收。

| 模块 | 当前实现 |
|---|---|
| 本机服务、认证、HTTP、WebSocket、会话代次 | Rust / Tokio / Axum |
| 界面、网络请求、按键语音与过期结果保护 | Rust / Yew，编译为WebAssembly |
| 摄像头发现与采集 | Rust / Windows Media Foundation / nokhwa，同一Rust程序的受控辅助进程 |
| 人脸检测 | Rust / ort，调用已核验的本地ONNX Runtime原生库及face专用ONNX |
| 多脸追踪 | Rust / edgefirst-tracker，真实ByteTrack的Kalman与两阶段匹配 |
| 麦克风 | Rust / CPAL / WASAPI，专属拥有者线程，20秒有界内存 |
| 本地识别 | Rust / whisper-rs，调用本地whisper.cpp与多语种ggml base权重 |
| 本地播报 | Rust直接调用Windows SAPI COM，使用已安装中英文声音 |
| 教学后端 | Rust直接管理官方Kimi Code2.3.1子进程，无shell或脚本中转 |
| 可选中英语义检索 | 显式启用的本地 BGE-M3 CPU 服务、SQLite 向量缓存与有界查询；失败回退词面召回 |
| 快慢记忆、来源、中文召回、删除 | Rust / rusqlite / SQLite，真实与模拟独立数据库 |
| 物理云台 | Rust状态与冻结保护；校准失败后禁止一切应用运动命令 |

所有应用和界面逻辑由Rust编写。CSS/HTML以及wasm-bindgen自动生成的浏览器桥接文件属于必要静态资产；官方Kimi CLI及本地推理引擎作为外部依赖使用。运行不启动Python、PowerShell、Node或npm来承载应用逻辑。

视觉、教学、记忆和音频执行链保持分离。采集只保留最新帧，帧与框共享frame_id；镜像仅影响显示。Kimi请求不阻塞视频。原生辅助进程用Windows Job Object管理，父进程退出时回收；启动管道握手保证设备只在Job绑定后才初始化。

教学每轮独立调用。Rust原生建立隔离CLI目录和只引用正常OAuth凭证目录的NTFS junction，不提取token。ACP 的 `extra_agent_dirs` 只指向当次隔离目录，避免外层 Git 项目根覆盖教学 profile；专用agent显式tools/subagents为空，并在提示前读取实际绑定证据。原生Job控制子进程树、输出有界、超时有界、JSONL严格解析。所有更新仅是候选，严格字段与来源校验后，还需实际显示确认才应用。

SQLite结构与原数据协议兼容。学习者档案、会话、活动、摄像头周期、临时track和request_id/generation分别管理。删除增加epoch，取消旧生成和音频，处理派生记忆、来源、快状态、近期对话和本应用拥有的CLI记录；不删除已有编程历史。新活动与恢复时，当前模块必须重新确认。

本机保护覆盖API、视频和WebSocket：仅监听127.0.0.1，随机HttpOnly/SameSite严格cookie，Host/Origin检查，无宽泛跨域。教学文字会发送远程Kimi；视频和声音不发送远程。

未来真实模块接口应由小组提供拓扑、设备周期、时间戳、模块连接、传感器读数和独立安全验证结果。当前仅接受人工报告和独立模拟，不开放真实硬件执行字段。情绪模型保持关闭。
