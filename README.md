# AI Tutor · Rust 本机工作台

应用服务、界面逻辑、摄像头管线、语音管理、Kimi 调度和快慢记忆均使用 **Rust**。界面由 Rust/Yew 编译为 WebAssembly；模型通过本地原生推理库运行。

这是围绕主控、电机、测距和灯等实体功能模块的教学原型。它帮助学习者描述目标、搭建、报告试验、观察和修改；不控制小车、不识别积木拓扑、不做人脸身份或情绪判断。

2026-11-03 软件优化覆盖重复启动保护、教学记忆事务、录音请求隔离、界面异步操作和视觉状态读取，详见 [本轮优化与验证](docs/OPTIMIZATION_REPORT_20261103.md)。历史硬件验收仍按各报告的原范围保留。

## 使用

1. 双击 **Start AI Tutor.cmd**。Rust 程序会打开已经认证的本机页面。
2. 在“教师工作台”创建或选择匿名档案；真实记录与独立模拟空间分别保存。
3. 需要视觉互动时开启摄像头，点击人脸框选择临时视觉目标。选择另一张脸不会更换档案；文字与语音教学可独立使用。
4. 输入目标或观察结果。模块清单由人工报告或教师确认；模拟清单需使用模拟档案。
5. 按住“说话”、松开转写，检查或修改后发送。只会朗读当前有效且已显示的教学回复；“停止”可中断。
6. 展开“学习记忆”检查来源、纠正或忘记。更正保留无关的当前事实；遗忘还会清理该档案的暂态对话和快状态，防止旧摘要重新引入被删除内容。
7. 点击“结束并保存”会先保存报告框里的观察，再将有来源的进展保留到下次。开始新活动会收尾旧活动；下次选择同一档案可召回历史进展，当前搭建仍须重新确认。
8. 双击 **Stop AI Tutor.cmd**，关闭本地服务并释放设备。关闭浏览器不会停止后台服务。

地址为`http://127.0.0.1:8765/`。如果提示需要启动凭证，请再次使用启动器；临时认证链接不要转发。

界面供使用者操作；助手使用本地命令行工具 `bin/tutorctl.exe` 读取状态和控制工作台，不代替使用者点击电脑界面。入口与命令见 [CLI 使用说明](docs/CLI.md)。

## 运行与构建

已构建程序放在`bin/ai-tutor.exe`。运行应用无需Python、Node、npm或开发服务器；保留正常登录的官方Kimi Code CLI作为教学模型后端。

```powershell
.\bin\ai-tutor.exe --root .
.\bin\ai-tutor.exe --stop --root .
.\bin\ai-tutor.exe --help
```

参数拼写错误会直接退出并显示帮助，不会误启动服务。真实 Kimi 无工具核验使用 `--verify-kimi`，它会通过已有登录调用远程模型；通常只在安装或核验失效后执行。

同一项目目录只运行一个服务：普通启动和 `--no-browser` 都会复用已运行的服务；更换端口不会创建第二份实例。并发启动或旧实例退出尚未完成时会提示稍候重试。`runtime/server.lock` 由系统自动释放锁定，文件留存是正常现象，无需删除。本机控制请求不使用代理，也不跟随重定向。

启动失败会显示提示，并尽可能在 `runtime/startup-error.json` 保存只含时间、错误类别和本地端口的诊断，不记录凭证、对话或私有 Prompt。端口被占用时，可先停止本项目旧服务，或用 `--port 8766` 指定其他本地端口。

默认浏览器打开失败时，启动器会尝试已安装的 Edge、Chrome 或 Firefox。仍失败会显示本地入口；已有登录的浏览器可直接访问。首次认证可明确指定浏览器，例如：

```powershell
.\bin\ai-tutor.exe --root . --browser "C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe"
```

浏览器路径应按实际安装位置填写。认证链接仅在本机打开，不写入诊断。`--no-browser` 供后台与自动检查使用，不自动打开浏览器或弹窗。

构建和检查使用本机已有Rust1.98.1、MSVC与Windows SDK：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-rust.ps1 test --locked -p ai-tutor
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-rust.ps1 build --locked -p ai-tutor --bin ai-tutor --bin tutorctl --bin ptz-read-probe --release
powershell -NoProfile -ExecutionPolicy Bypass -File frontend/build.ps1
```

首次准备环境可运行 **Install AI Tutor.cmd**。依赖锁在`Cargo.lock`；模型来源、版本和摘要在`models/`。新机器需匹配Windows编译工具位置，见`.cargo/config.toml`与构建脚本。不会升级或改写全局Kimi配置。

更新也使用 **Install AI Tutor.cmd**：先构建新程序和独立界面输出，再请求本项目现有服务正常停止并等待释放资源；停止失败就取消更新，不强制结束进程。安装文件先暂存并校验，再替换程序与界面；替换失败会恢复已更换文件。旧文件保留在 `runtime/program-backups/`，学习数据 `runtime/data/` 不参与替换。更新成功后重新双击 **Start AI Tutor.cmd**。安装末尾会进行真实 Kimi 无工具核验；若登录失效，完成正常登录后重新执行 `--verify-kimi` 即可，已安装程序与学习记录保留。

安装部署逻辑可用 `powershell -NoProfile -ExecutionPolicy Bypass -File scripts/test-install-rust.ps1` 在独立临时目录检查；该检查不停止实际服务、不读取设备、不调用远程模型。

## 当前边界与验收

- 真实OBSBOT Tiny3发现、原生视频、face专用YOLO ONNX、ByteTrack与目标选择已经实现。多人交叉/遮挡和真实拔插需要单独现场验证。
- Rust原生Kimi2.3.1无工具连通、诱导读取测试文件、来源校验、模拟小车多轮教学、快慢记忆与隔离已实际测试。
- 本地语音使用WASAPI、whisper.cpp多语种base和Windows已安装中英文声音。2026-11-01用户确认“转写与播报都正常”（source_type=user_confirmation），基本成人按键说话、检查或编辑转写、发送并听回复的流程与可听播报通过。英语自然语音、打断、回声及播报中切档案仍待现场确认，G01中英文全项尚未通过；合成样本与历史自动探测保留原证据范围。
- **物理云台暂缓**：2026-11-02 已取得方向标定及有限跟随软件/读回证据；新版实体水平跟随的人眼确认、完整物理急停、拔插和长期运行仍未验收。按最后使用安排保持关闭，详细范围见 [跟随修复记录](docs/PTZ_TRACKING_REPAIR.md)；此前失败记录保留。
- 教学文字发送到远程Kimi；摄像头画面、录音和本地识别不发送远程。没有实际token统计时显示“未知”。

完整实测状态见`ACCEPTANCE_REPORT.md`及`docs/RUST_*`报告。应用仅监听本机，HTTP、视频和WebSocket均受本地认证与来源检查保护。默认不保存视频、人脸裁剪或原始录音；数据保存在`runtime/data`，不自动备份。

旧Python/TypeScript原型已移至`archive/pre-rust-reference`，不参与生产构建或运行。原始用户规格完整保留在`implementation-package/AI_Tutor_Codex_Implementation_Package`。

## 开源许可证

本项目采用 [MIT 许可证](LICENSE)。模型权重、运行数据、私有提示词和编译产物不包含在源码仓库中；模型来源、版本和摘要见 `models/` 下的清单文件。
