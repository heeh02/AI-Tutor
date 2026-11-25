# AI Tutor 本地功能积木学习工作台

在本机摄像头画面旁，通过真实Kimi教学对话维护活动与快慢记忆。主控、电机、测距、灯是具有实际功能的模块；本程序不控制小车。

## 启动与停止

当前电脑已安装项目环境、模型和构建好的界面。

1. 双击本目录 **Start AI Tutor.cmd**。启动器打开已认证的`http://127.0.0.1:8765/`，服务窗口隐藏。
2. 进入“教师工作台”，创建或选择匿名档案。真实与“独立模拟”使用不同数据空间。
3. 开启摄像头。点脸框只改变视觉目标，学习者档案要另行切换。
4. 在模块面板记录人工报告/教师确认；模拟模块只写入模拟档案。
5. 输入想法或按住“说话”，松开后修改转写再发送。收到有效回复后可自动本地朗读，也可按“停止”打断。
6. 展开“学习记忆”查看来源、纠正或忘记。**忘记会同时清理该档案的暂态对话和快状态**，避免旧摘要重新引入内容。
7. 结束本次活动，再创建新活动；下次选择同一档案可以召回历史，但须确认当前搭建。
8. 双击 **Stop AI Tutor.cmd**，释放设备和本应用子进程。不要用关浏览器代替停止本地服务。

如果只打开地址看见“请用启动脚本打开”，再次双击启动器即可；不要转发含token的临时启动URL。

## 工程入口

```powershell
.\.venv\Scripts\python.exe scripts\start.py --no-browser
.\.venv\Scripts\python.exe scripts\stop.py
.\.venv\Scripts\python.exe -m pytest -q
```

干净安装使用 **Install AI Tutor.cmd**，会建立项目环境、安装锁定依赖、下载并校验模型、构建界面、通过正常Kimi登录引用完成无工具探测。不会升级Kimi或修改全局配置。要求Windows11、Python3.10及已有Kimi Code2.3.1 OAuth登录。Node/pnpm构建器会先发现本机或随Codex提供的运行库。

## 能力与限制

- 真实OBSBOT Tiny3视频，第三方YOLO-Face ONNX、ByteTrack与教师选择目标。没有跨会话身份或语音说话者识别。
- Kimi Code2.3.1真实远程教学，专用无工具agent；摄像头画面与声音不发到Kimi。
- 本地Whisper base多语种ASR和Windows已安装中英文TTS。自动识别可能出现同音字，应检查转写。
- 快状态、中文相关召回、带来源的慢记忆、重启续接、跨档案隔离和删除epoch。
- **物理云台保持关闭，尚未转动验收**。本机缺可调用官方SDK/Center OSC，具体步骤见`docs/PTZ_REPORT.md`。
- 不识别积木连接，不执行小车，不加载情绪模型，不开放公网。
- 测试报告区分真实Kimi/摄像头、自动脚本人工报告、合成语音、真人语音、未测试项。查看`ACCEPTANCE_REPORT.md`，不要把代码存在当作设备验收完成。

原始规格与Schema完整保留在`implementation-package/AI_Tutor_Codex_Implementation_Package`。本地数据在`runtime/`，不提交源码；不默认保存视频、人脸裁剪或原始音频。没有自动备份或导出功能。
