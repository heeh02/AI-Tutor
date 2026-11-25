# Rust 视觉与 PTZ 实现

2026-11-01。本文件单独记录 Rust 迁移后的结果，不把早先 Python 采集性能当成 Rust 性能。

2026-11-04 更正：下文的性能数字和“PTZ保持冻结”描述属于11月1日版本。当前源码已有显式启用、标定绑定和有限位置控制的 OSC 跟随实现，不能再用历史段落声称程序没有运动能力。当前 Goal 仍遵守用户暂缓实体 PTZ 的要求；本轮相机诊断不复制启用/标定配置，不选择目标、不调用运动入口。当前跟随实现见 `PTZ_TRACKING_REPAIR.md`；新的相机实测以 `GOAL_PROGRESS_20261104.md` 中对应检查点和报告为准。

## 生产路径

`rust/ai-tutor/src/vision.rs` 实现原生 Windows Media Foundation 设备发现与采集、RGB letterbox、ONNX 人脸推理、NMS、真实 ByteTrack、JPEG 编码、目标选择和设备进程生命周期。应用不启动或导入 Python，也不依赖 Python 服务。摄像头辅助进程是同一个 Rust 可执行文件的专用模式。

采集用 nokhwa 0.10.11 的 Windows MediaFoundation 后端；设备ID来自 `MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK`。按设备名称/稳定链接枚举后再打开，未硬编码摄像头0。Media Foundation 的符号链接使用 e5323777 设备接口类别；与旧 DirectShow moniker 的65e8773d类别字符串不同是预期行为。新选择保存在 `runtime/rust_camera_device.txt`，每次打开按当前原生枚举重新匹配。

推理用 ort 2.0.0-rc.10，动态加载项目内 `models/native/onnxruntime.dll`（Microsoft ONNX Runtime 1.23.2，原生C++库，不是 Python 解释器）。ONNX Runtime 运行遥测明确关闭。RGB缩放、补边、CHW/float32归一化、face-only输出解析、坐标还原和NMS在Rust完成。摄像头提供MJPEG时直接使用同一帧的原始JPEG做预览，避免重复编码；其他格式由Rust编码JPEG。操作系统的MediaFoundation和原生ONNX库是运行依赖。

人脸模型仍是 [Lindevs 专用 YOLOv8n-Face](https://github.com/lindevs/yolov8-face)，不冒称 Ultralytics 官方人脸模型。加载前核验 SHA-256、嵌入的 `{0:'face'}` 类别、输出 `[1,5,8400]`；普通COCO person模型不能通过该检查。

跨帧关联使用 [edgefirst-tracker 0.38.2](https://github.com/EdgeFirstAI/hal/tree/main/crates/tracker) 的实际 ByteTrack，包含常速度Kalman预测、两阶段高/低置信检测关联和LAPJV分配，不是把IoU最近邻重命名。参数显式配置：高置信0.4、IoU0.25、更新参数0.25、失踪生命周期600ms；3次匹配后显示候选，ReID关闭。track_key由本次相机UUID和跟踪UUID组成，重启不会复用旧绑定。

## 生命周期与同步

- 采集线程只保留一个最新RGB帧；推理处理当前最新帧，不积压历史。
- 当前JPEG与脸框、frame_id、camera_session_id来自同一帧。服务端始终提供原始坐标；镜像属于界面显示转换。
- 专用Rust进程通过有界二进制协议传回结果：元信息最大32KiB、JPEG最大5MiB。日志单独排水且只保留最多约4KiB的诊断尾部。
- 无结果超过10秒时，父进程只终止本应用的采集子进程；最多三次启动尝试。关闭/切设备会终止旧进程、清空画面和绑定；旧generation结果不能写回。
- 采集和发现辅助进程均加入Windows匿名Job Object，使用`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`，句柄不可继承且只由父进程持有。Job配置/关联失败会回收辅助进程并拒绝继续；关联成功后，即使父程序崩溃且相机驱动卡住，操作系统也会随父句柄关闭终止辅助进程，不依赖画面输出触发BrokenPipe。依据：[Microsoft Job Objects](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects)及[Kill-on-close定义](https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-jobobject_basic_limit_information)。
- 辅助模式进入任何相机/ONNX初始化前等待父进程stdin启动标记；父进程只在Job关联成功后放行。父进程若在spawn与Job关联之间退出，管道EOF使辅助进程在获取设备前结束；无效标记同样拒绝初始化。
- 短暂丢失后保留600ms，过期画面拒绝新选择；脸框重叠造成关联歧义时解除绑定并要求重新选择。点击人脸不会切换学习者档案。
- 摄像头、检测和预览与Kimi请求分开；没有逐帧远程调用，没有保存原始视频/脸裁剪。

主程序必须在建立工作台前处理两个内部模式：`--camera-discover` 调用 `vision::discovery_worker()` 并输出JSON；`--camera-worker <root> <stable_id>` 调用 `vision::camera_worker()`。辅助进程启动使用固定自身程序和参数数组，不经过shell。HTTP层调用可能阻塞的发现/打开操作应放入阻塞工作池。

## 资产校验

| 资产 | SHA-256 |
|---|---|
| models/yolov8n-face-lindevs.onnx | 8d0bfb0c3383c5bd7a78dd24ef79a21e2aa456619b6ab5e53867092d1c7dc414 |
| models/native/onnxruntime.dll | 2bc04b05efbff97049132939640963772cc16225ba1c7f1388993487c8775492 |
| models/native/onnxruntime_providers_shared.dll | 6adee08db23ba190fd1061ee99c32a5a802cac1e3576defaac820c157294c138 |

DLL复制自本机先前通过官方PyPI安装的onnxruntime 1.23.2二进制发行内容，放在独立项目目录；生产加载路径不访问`.venv`。主DLL和模型摘要在Rust加载时强制核验。ONNX Runtime许可证MIT；ort为MIT/Apache-2.0；nokhwa、edgefirst-tracker为Apache-2.0。权重仓库MIT与ONNX嵌入AGPL-3.0分别保留，未声称MIT覆盖权重及WIDER FACE数据条款，见`models/manifest.json`。

## Rust验证结果

| 检查 | 结果 |
|---|---|
| 生产vision.rs/ptz.rs原样包含的独立Rust编译 | PASS |
| 12个Rust测试：真实ONNX DLL/face模型加载及空图推理，ByteTrack低置信恢复，letterbox/NMS，显式选择/旧帧/旧epoch/歧义拒绝，IPC大小限制，PTZ冻结，原生Job关闭后回收测试子进程，启动门控EOF/无效标记拒绝 | PASS |
| 实际MediaFoundation枚举到OBSBOT Tiny 3 StreamCamera及稳定符号链接 | PASS，证据runtime/rust_camera_discovery.json |
| Rust真实实时采集和重开 | PASS：15秒41个不同处理帧；重开7秒18帧；1280×720、MJPEG，新camera_session_id，均无错误且已释放，证据runtime/rust_camera_probe.json |
| 两人同框、遮挡、USB拔插、权限拒绝 | NOT_RUN，合成框测试不替代设备验收 |

原生模型测试使用真实ONNX运行库进行推理，不是mock；其输入为空图，不证明当前摄像头中已有人脸。独立验证临时harness位于被忽略的runtime目录，包含的是生产模块源码；最终启动入口由主程序提供。

首次真实硬件探测使用未优化debug构建，完整处理约337ms/帧、约2.8处理fps，帧龄中值371–378ms；没有检测到脸。该结果证明原生实时链路与释放重开，不满足最初的帧率目标。探测后已改为透传相机原生MJPEG、分开记录`inference_ms`与`processing_ms`，并加入Job回收与启动门控保障，12个Rust测试重新通过；应以最终生产构建再测性能，不能预先宣布加速结果。Job测试只运行本应用专门创建的本机回环ping子进程，未再打开实际相机，也未故意使工作台崩溃。

Nokhwa的当前格式元数据曾报告fps=1，但同次相机帧号15秒推进至412，不能把该元数据当成测得的采集帧率。当前状态将它明确命名为`driver_reported_fps`，并单独报告真实采样计数得到的`capture_fps`与处理`fps`；支持格式列表最多32项，超过时标注截断。

## 11月1日历史版本的 PTZ 冻结状态（已被后续实现取代）

`rust/ai-tutor/src/ptz.rs` 仅以serde读取以前的原生只读能力报告与失败标定证据，不发送任何设备命令，也没有IAMCameraControl.Set或速度调用。所有手动/自动启用入口均拒绝，stop/close不发送角度；删除报告也不会自动解锁运动。

历史原生接口读取支持Pan -130..130度、Tilt -90..90度。单次1度Pan测试曾观察到Pan 7→8→7，但没有Tilt命令时Tilt 10→1；原因未核验，详见`PTZ_REPORT.md`。Rust状态保留`low_amplitude_calibration=FAIL`、`external_follow=BLOCKED`、`vendor_auto_state=unknown`。缓存中的“current”是历史读数，不声明为此刻位置。当前版本未重做物理测试，不能交付PTZ_VERIFIED。
