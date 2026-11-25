# 本地摄像头与人脸视觉验收

2026-11-01；实际 Windows 11 Pro 10.0.26300，Python 3.10.11，Intel i5-14600K；采用 CPUExecutionProvider，不假定 RTX 5070 的 CUDA 栈可用。

## 已实测

- `python scripts/probe_camera.py --seconds 15`：PASS。原生 DirectShow 枚举到唯一 OBSBOT Tiny 3 StreamCamera；由设备路径重新映射当前 OpenCV 索引（本次为 0），不是预设索引。
- 协商 1280×720 / 30.00003fps，15 秒中获得 112 个不同的实时检测帧，frame_id 连续推进；检测中值 31ms，采集至服务取样帧龄中值 78ms、p95 109ms。启动初次依赖导入包含在总时长内。详细证据 `runtime/camera_probe.json`。
- probe 的持续拍摄没有保存原始视频、截图、脸裁剪或人脸身份数据；关闭后 capture owner process 退出并释放设备。
- 原生枚举稳定设备路径含 VID/PID；每次设备重开重新枚举并匹配该路径。路径可能因 USB 端口/驱动变化而变化，跨 USB 端口稳定性尚未实测。
- 权重 SHA 与作者发布值一致，ONNX 嵌入类别 `{0:'face'}`、输入 `[1,3,640,640]`、输出 `[1,5,8400]` 已直接读取验证。模型仅检测脸，不识别积木、情绪、身份或发言人。
- 9 个测试通过：横竖画幅 letterbox 坐标还原、镜像坐标、显式选择不追逐最大脸、旧帧与旧连接周期拒绝、关联歧义拒绝和真实模型完整性。

## 运行机制

采集在可终止子进程，最新帧队列容量 1；推理在独立线程，以最多15处理帧/秒运行；JPEG 与脸框来自同一 `frame_id`。驱动挂起超过8秒时最多重启两次；正常拔插以1秒间隔重新发现。切换/重连清除视觉绑定；画面过旧禁止新绑定。目标脸接近重叠时解除绑定，消失超过0.6秒重新选择，不作身份承诺。ByteTrack 参数显式锁定，ReID 关闭。真实硬件、断线、权限失败与内存长跑需分别记录。

## 尚未实测

本次相机画面没有出现可检测脸（max_faces_observed=0）。两人真实同框选择、真实遮挡交叉、USB拔插、相机权限拒绝、被别的程序占用、30分钟连续运行为 NOT_RUN，不能把合成框测试记为这些设备测试通过。当前 preview 与检测同频，15fps上限，未达到规格预览≥20fps的工程目标。

完整支持格式枚举尚未实现，列表明确显示“仅打开后协商格式”；目前已验证的格式是1280×720@30，不声称测试了其他格式。

## 来源与许可

- [Lindevs 作者仓库](https://github.com/lindevs/yolov8-face)：第三方专用人脸模型，作者提供 ONNX 与 SHA-256；本地文件与发布摘要一致。
- [仓库许可证](https://github.com/lindevs/yolov8-face/blob/master/LICENSE)：MIT。ONNX 本身嵌入 `AGPL-3.0 License`；分开记录，不用仓库 MIT 覆盖权重声明。模型训练使用 WIDER FACE，官网访问超时，其完整再利用条款尚未核验；未下载训练数据。
- [cv2-enumerate-cameras 1.3.0](https://pypi.org/project/cv2-enumerate-cameras/1.3.0/)：提供 DirectShow 的对应索引、名称、设备路径、VID/PID，MIT。
- [Supervision 0.27.0 ByteTrack](https://github.com/roboflow/supervision/blob/0.27.0/supervision/tracker/byte_tracker/core.py)：真实 ByteTrack 实现，MIT；本项目未自创关联算法冒称 ByteTrack。

模型下载、接口布局、摘要、各项许可状态在 `models/manifest.json`。官方文档与本机实测分开；没有写入厂商控制命令。
