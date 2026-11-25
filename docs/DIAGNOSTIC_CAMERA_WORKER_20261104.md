# 同实例联合诊断的相机进程入口

生产 `CameraService` 仍调用当前主程序的 `--camera-discover` / `--camera-worker`。库测试exe没有这两个入口；直接打开相机会把测试harness误当成相机子进程。因此新增仅在 `cfg(test)` 存在的、各fixture独立的入口选择。

测试fixture可明确提供 `runtime/diagnostic-camera-worker.json`，包含 `protocol_version:1`、主程序exe的绝对路径 `executable` 和实际文件 `sha256`。每次启动前核对字节摘要，再使用原有Windows Job、stdin启动闸门和子进程收尾。配置不存在时仍使用current_exe；release构建不读取该文件。没有全局环境替换或HTTP配置入口。

`vision_test_worker.rs` 的回归验证：两个CameraService根目录不共享覆盖；已声明的程序字节改变时拒绝。测试使用普通文本文件而不执行它。该检查已纳入 `workspace-thirty-ninth.log` 的769项通过，不能据此宣称摄像头已运行。

当前两轮语音诊断不写此配置、不打开相机。此接点供后续同一个Workstation的持续摄像头/语音/教学负载使用；相机必须由该实例管理，不用另起一个主程序冒充同实例联合测试。不存画面，不选择人脸或目标，不启用云台。真正30分钟联合运行及相机回收仍需独立实际结果。
