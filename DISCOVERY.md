# 实机环境发现

## 当前 Rust 交付补充

用户最新要求采用全Rust应用。已核验 Rust1.98.1、Cargo1.98.1、MSVC14.44.35207与Windows SDK10.0.26100.0；已补装wasm32-unknown-unknown编译目标，未升级全局Rust。服务端为Rust/Tokio/Axum，界面为Rust/Yew WebAssembly，摄像头为原生Media Foundation，音频为Rust/WASAPI/whisper.cpp/SAPI，记忆为Rust/rusqlite。运行入口为bin/ai-tutor.exe。

Rust版独立完成真实摄像头连续读取与重开、原生Kimi无工具和canary、多轮教学与实际进程重启、真实麦克风采样与SAPI停止，以及本地中英文合成样本识别。最新证据以docs/RUST_*及ACCEPTANCE_REPORT为准；下列Python环境盘点属于前期发现，不代表当前应用运行依赖。

物理云台后续发现（2026-11-02）：原生IAMCameraControl读范围可用；OBSBOT Center2.0.15.186官方OSC已实测，双轴方向标定通过。最终Rust版本已验证有限目标到位、短暂丢失后重新核验恢复、本地取消与重新启用；当前物理水平跟随的用户确认仍待取得。旧版跟随和单次标定FAIL保留为历史。最新范围见docs/PTZ_TRACKING_REPAIR.md及runtime/ptz-follow-release-manifest.json，不能把控制器或读回PASS等同完整实体跟随验收。

日期：2026-11-01（Asia/Shanghai）。本机查询，不根据照片推断。

- Windows11 Pro64位，系统版本10.0.26300。
- Intel Core i5-14600K，约32GB内存，Intel UHD770与NVIDIA RTX5070。
- Python3.10.11；已建立项目`.venv`，没有升级或替换全局Python。
- Node24.22.1，使用Codex bundled pnpm11.25.0，前端构建在项目目录。
- Kimi Code2.3.1，`C:/Users/RBMS/.kimi-code/bin/kimi.exe`；沿用已配置OAuth，未导出token、未修改全局配置。
- 原生DirectShow发现OBSBOT Tiny3 StreamCamera，VID3564/PIDff02。稳定设备路径在本机能力JSON中，启动时重新枚举映射，不保存索引假定。
- 已协商1280×720约30采集fps；CPU上ONNX face推理和ByteTrack处理，实际检测速度单独测量，不宣称GPU已启用。
- 麦克风：OBSBOT Tiny3，WASAPI48kHz单通道采集成功。
- 已安装本地声音：Microsoft Huihui Desktop(中文)、Zira Desktop、David Desktop(英文)。
- 云台：SDK尚未取得；已通过官方Center OSC进行有限位置与状态实测。同采集owner原生Get已与旧DirectShow读数交叉核对；可用连续实体跟随及完整故障停止仍待验收。

检查范围限工作区、工具安装目录及相关设备列表；没有扫描用户私人文档。工作区原本不是Git仓库，没有覆盖原有源代码或私有Prompt。

完整设备/视觉与语音证据见 `docs/VISION_REPORT.md`、`docs/ASR_MODEL_REPORT.md`、`docs/KIMI_REPORT.md`、`runtime/audio-probe.json`。
