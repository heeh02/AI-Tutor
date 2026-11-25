# 相机与语音服务并存、周期预热诊断

入口为 `scripts/validation/camera_speech_coexistence_soak.py`，方法为相邻 `camera_speech_coexistence_method_v1.json`。这是同一个实际主程序管理的相机 worker、ASR 和 TTS 服务并存测试，不是完整教学或连续语音轮换验收。旧 camera-stream 脚本与方法未改。

默认测量 1800 秒。相机先打开，持续使用实际 `/api/video/stream` 接收并在内存解码 JPEG；两项语音服务首次同时 ready、warmup_completed、active 后才开始测量计时。此前相机准备期单独统计。在第 300、600、900、1200、1500 秒显式停止并重新准备两项语音服务，保持同一主程序、相机和视频连接。若重启跨过下个周期或测量结束，则失败，不叠加重试。少于 1800 秒均标为 `short`，短测成功不能写为完整 30 分钟通过。

## 实际负载与不能宣称的内容

- 相机是当前 Rust 主程序的原生采集、YOLO/ByteTrack、最新帧槽和二进制 WS 链；客户端复用已冻结 packet 校验和统计函数，不保存 JPEG、图像、脸框或轨迹身份。
- ASR 使用主程序托管的 Zipformer，实际执行既有一秒合成静音 warmup。没有向主程序送说话 WAV、没有使用真实 capture pump，也没有转写准确率结论。
- TTS 使用主程序托管的 Cosy 或 Qwen，实际生成既有固定中英文 warmup 文本并将 PCM 排空到内存 sink。每次两种语言必须有非零样本数，且 transport 样本数一致。没有打开扬声器，没有教学回复或听感结论。
- 测试期间服务其余时间只是驻留；这不是 ASR/TTS 连续满载，也不是 Kimi→教学→真实播放全链路。没有新增生产诊断 API，不读取托管语音的私有 token，不从公开旧描述符连接另一服务冒充当前主程序。
- 视频指标从 `camera.frame()` 返回开始，到 Python 接收和 Pillow 解码完成；不是传感器曝光、浏览器 paint 或屏幕呈现。Windows 归属进程记录 RSS 与累计 CPU 时间，不代表 WSL 模型的 Linux RSS 或 GPU 显存。

## 根代理运行入口

先确认没有另一托管 GPU 语音实例占用，明确指定当前检查点程序、已构建 UI、固定 ASR Python 与已选摄像头。脚本不会替操作者停止旧服务、安装环境或下载模型。示例短测：

```powershell
.venv\Scripts\python.exe scripts\validation\camera_speech_coexistence_soak.py --real-camera-local-synthetic --root . --exe target\release\ai-tutor.exe --ui frontend\dist --python-executable runtime\speech-lab\.venv\Scripts\python.exe --engine cosy --distribution Ubuntu --references-manifest runtime\goal-baselines\20261104\cosyvoice3-preparation\reference-audio.json --device-id-file runtime\rust_camera_device.txt --duration-seconds 60 --output runtime\goal-baselines\20261104\camera-speech-coexistence-short-first
```

`--single-camera` 可替代设备文件，仅本次原生枚举恰好一个设备时允许使用。多个或没有设备时退出，不猜默认设备。默认不传时长即 1800 秒；为验证一次周期而做缩短运行时，须给第 300 秒后的 stop/retry 留出实际准备时间。允许的 10–1800 秒参数不等于任意长度都能完成晚于 300 秒启动的周期。

此命令涉及两个不同的 Python 环境，不能互换：

- 命令开头的根目录 `.venv\Scripts\python.exe` 运行诊断接收器，提供 websockets、Pillow、psutil。
- `--python-executable runtime\speech-lab\.venv\Scripts\python.exe` 只交给主程序作为托管 ASR 的解释器，提供 NumPy、sherpa-onnx；它不负责运行诊断脚本，也不需要混装视频诊断依赖。

TTS 继续使用既有 WSL 环境指针，由主程序托管；以上两个 Windows Python 参数都不会替换或改造它。

Cosy 参考清单必须为已核验的 `synthetic_sapi_adult_voice` 中英样本，各自 SHA 校验，引用原文件，不播放。参考声会影响生成自然度，不能当真实人声质量验收。Qwen 不使用参考清单。

只使用已有依赖 websockets 15.0.1、Pillow 12.3.0、psutil 7.0.0。程序资产从源码复制，模型只为新 fixture 建硬链接，推理缓存写新 fixture。源模型和程序前后核对哈希；不复制生产 DB、语音服务描述符或凭证。源模型文件预先读取/哈希，因此启动时间不是冷缓存基准。

## 证据与退出

`video-frames.jsonl` 仅保存白名单数字/布尔/阶段名；`state-resources.jsonl` 保存安全状态与本次主进程/后代进程的 PID、创建时间、RSS、累计 CPU。`report.json` 单列初次与每次 warmup、周期安排/实际起止、视频准备期/测量期分布、格式变化、最终清理和 short/full。失败保留已有行及阶段状态，不自动重发请求。

最终停止接收，分别向同一 main 请求停止 ASR/TTS并核对归属清理回执，再 camera.close 并核对已观察 worker 退出；最后主程序 HTTP shutdown、exit 0、无强制 main 退出才通过。服务 stop 允许其既有的有界组清理，实际 `forced` / `forced_group_stop` 回执保留，不能把它们改写为自然退出。未知清理或任一错误均使运行失败。只删除本 fixture 的精确 policy、已核验设备选择文件及既有 helper 归属匹配的主程序凭证；不递归删除 fixture，不按端口或进程名结束其他进程，不使用 WSL shutdown/terminate。

源 `bin/ai-tutor.exe`、策略、描述符与相机配置前后哈希不变。音频录制、播放、目标选择、PTZ 和远程教学入口不在 HTTP 白名单。应用未发 PTZ 请求不等于实体云台物理静止。

方法无预设帧率/延迟通过阈值，性能是观测值。五项纯回归已通过，覆盖计划周期、真实 warmup 样本回执、禁止入口、停止归属和准备期/测量期统计分离。该测试未启动服务、模型或设备。

## 首次启动前失败记录

`runtime/goal-baselines/20261104/camera-speech-coexistence-first/report.json` 原样保留。第一次误用 ASR 专用解释器运行诊断，依赖检查出现 `PackageNotFoundError`；外围用时 0.194s，未进入资产 stage，`main` 为空、worker 观察为空，没有启动主程序、摄像头或模型。它不是一次已运行联合负载的性能失败。

该版依赖检查早于受保护文件基线读取，导致 `finally` 又把缺少 before 误报为 `protected_source_state_changed`；这条不能作为实际文件变化证据。根代理已将基线读取移到 `try` 开头、依赖检查之前，保持真实前后比较与原始报告不变，再用根目录 `.venv` 运行第二次。此修补不改变冻结方法、时长或通过条件。

## 第二次实际短测

`runtime/goal-baselines/20261104/camera-speech-coexistence-second/report.json` 的 SHA256 为 `f5ee58e568d3829c0b11aae7dba7b363317cf143bf4171e3f379c0d075b5eb26`。实际使用脚本 SHA256 `7892ca31f9058aaeff761ef8fbc54932240e52d4425736a078fc5b4b97148fbf`、native19 exe SHA256 `9e5a103c70f95d489fda39e1f9dbe78463c4dfa01818a9df8c8211194acabcc2`；冻结方法仍为 `eb4538ca07bf27117b012051c9cf5306fb79509afd246ba0f1ee5f1421224369`。

第二次 exit 0、`passed=true`、`run_kind=short`，外围总耗时 189.882s。首次准备耗时 126.552s，包含资产核验/stage、主程序启动和服务准备；其中相机实际首末接收跨度为 123.276s，不能把两者当作同一分母。暖后测量墙钟为 60.003s，首末视频帧接收跨度为 59.998s。

| 视频阶段 | 帧数 | 接收 fps | 捕获返回至 Pillow 解码完成 P95 | 最大值 | 无效时钟帧 |
| --- | ---: | ---: | ---: | ---: | ---: |
| 准备期 | 2869 | 23.265 | 88.29ms | 385.78ms | 0 |
| 暖后并存 | 1568 | 26.118 | 78.84ms | 98.24ms | 0 |

实际格式均为 1280×720 MJPEG、JPEG 全部复用、零人脸/轨迹；请求 30fps，驱动格式元数据仍报告 1fps，与上述实测帧率分别保留。ASR 只完成既有的一秒合成静音预热。TTS 中英文 warmup 分别收到 69120、90240 个真实 PCM 样本并验证 transport 数量一致，没有播放或保存音频。

主进程 142116 正常 HTTP shutdown、exit 0、未强制结束；camera worker 148548 已观察且 close 后不存活。ASR 解释器/launcher 两个 Job 均空、解释器退出、`forced=false`。TTS Linux 清理和客户端卸载均确认，bridge/service exit 0、`forced_group_stop=false`。已安装程序、原策略/描述符与源资产保持原哈希；fixture 的主程序凭证、已知 policy 和设备选择文件已移除。

独立审计 `runtime/goal-baselines/20261104/camera-speech-coexistence-audit-first.json` 从 4437 行视频记录重算各阶段全部 nearest-rank 分布、帧率、P95、时钟分母、宽高、JPEG 复用与 capture ID 空洞，均与报告一致；另核对 179 行状态资源观察和各项清理回执，受保护文件哈希再次只读核对一致。pixel_format/协商帧率仅存于报告格式观察，未逐帧重复保存；进程清理依据本次真实运行回执，审计没有重新启动或重新测量设备。

这次 **没有到达 300 秒周期，没有 stop/retry 周期结果，也没有完成 30 分钟**。暖后驻留 60 秒不等于持续语音识别、持续 TTS、教学模型负载或完整语音轮换。没有物理声音、浏览器 paint、跟踪质量或真人噪声验收结论。
