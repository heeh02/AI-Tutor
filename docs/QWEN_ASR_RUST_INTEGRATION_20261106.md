# Qwen1.7B 终稿 ASR 的 Rust 接入记录（2026-11-06）

**后续状态：**真实工作台启用/停用、推理中协作取消、ASR/TTS共驻与共享准入已通过独立验证，最新bundle为6文件（增加共享`gpu_admission.py`）；随后运行环境绑定和Stop响应修复的workspace为914通过。详见 [双模型共驻记录](QWEN_GPU_PAIR_20261106.md)。下文保留此前341文件/27文件候选的历史口径，不能将其未验事项视为最新状态，也不修改历史测试结论。

**独立 WSL Qwen 服务已接入 Rust client、生命周期 runtime、AudioService 终稿选择及 UI 状态；归档 Rust 程序的实际 GPU 文件诊断通过。** 本记录对应冻结的 **341 个源文件**，不是主目录 release 已部署或真实麦克风验收声明。[源码/二进制绑定](../runtime/goal-baselines/20261106/final-asr-integration-first/binaries/manifest.json)

## 稳定安装与启用

安装器将 `scripts/final-asr/` 中五个文件一起部署到 `bin/final-asr/`：`managed_final_asr.py`、`qwen_final_engine.py`、`qwen_final_service.py`、`qwen-model.json`、`qwen-sdk.json`。固定清单从 bundle 同目录读取；安装不复制权重、Python 环境、缓存、用户 policy 或凭证。模型仍为 `models/speech/Qwen3-ASR-1.7B-7278e1e7`，PyTorch BF16、SDPA、batch=1，无 aligner/VAD/热词。[安装器](../scripts/install-rust.ps1)、[bundle 说明](../scripts/final-asr/README.md)

新增 `runtime/local-final-asr-policy.json`。文件缺失或 `{"protocol_version":1,"enabled":false}` 时默认停用；本次没有改写用户现有 policy。下面仅示例本机已实测的环境配置，不能将其当作已经启用：

```json
{
  "protocol_version": 1,
  "enabled": true,
  "managed": {
    "distribution": "Ubuntu",
    "python_executable": "/mnt/c/Users/RBMS/.cache/ai-tutor-asr-17b-env-20261106/bin/python",
    "cache_directory": "/mnt/c/Users/RBMS/.cache/ai-tutor-asr-17b-runtime"
  }
}
```

Workstation 启动时读取 policy；显式启用才准备服务。`loading → warming → warm_ready` 分别表示加载、真实预热和可接请求，模型加载完成不能代替预热。UI 显示精确转写的准备/预热/启用/停用状态，提供重试和停止；准备过程不打开麦克风。[runtime](../rust/ai-tutor/src/final_asr_runtime.rs)、[UI 状态](../rust/tutor-ui/src/local_asr_status.rs)

录音开始时冻结当时的终稿 client，包括“没有 client”的情况；后续准备完成不会中途接管旧录音。收音后优先使用经实例、request UUID、generation、模型身份和截断信息校验的 Qwen 非空终稿；空结果、失败或超时回到既有 Zipformer/Whisper 路径。只提交一次终稿，仍 `requires_confirmation=true`。CPU partial 通道保持独立；本次实际诊断未启用该流式通道。**Qwen 此路径是全句离线终稿，不是 Qwen streaming。**[终稿选择](../rust/ai-tutor/src/audio/final_transcript.rs)、[HTTP client](../rust/ai-tutor/src/final_asr.rs)

## 实际诊断结果

使用真实 owned WSL 服务和归档 Rust executable；1 次预热、4 个固定原 PCM direct 请求，以及 1 次 AudioService fake capture，均无重试、无参考文本或语言提示。没有打开输入/输出设备。[实际报告](../runtime/goal-baselines/20261106/final-asr-integration-first/actual-first/report.json)

| 项目 | 实测及口径 |
| --- | --- |
| 首次完整准备 | **108.311 秒**，包括启动、校验、加载和预热；不是每次识别时延。 |
| AudioService 短答 `00167` | 700ms 已有音频通过假录音所有者进入真实 Rust→HTTP→GPU 路径，输出 **`No.`**；内部状态 `asr_ms=185ms`，完整 stop 调用 **200ms**，无回退且需用户确认。 |
| 四个 direct 请求 | `00025/00007/00009/00167` 分别 **535/532/356/140ms**，为 HTTP 调用到 final；均与该模型全量语料输出一致。 |
| 整卡采样峰值 | **6355MiB**，含桌面等负载；不是模型独占显存，也非保证捕获瞬时峰值。 |
| 正常清理 | service 和 WSL bridge 均 exit 0，`forced_group_stop=false`，Linux cleanup 确认；精确自有进程组复查无残留，client 已清除。 |

AudioService 使用 PCM→float→当前 live 量化；报告记录预期 wire hash，但没有独立截获实际 wire payload。上述 185/200ms 不能称麦克风松键到 UI 显示时延，四条样本也不能代表总体 P95。[完成复核](../runtime/goal-baselines/20261106/final-asr-integration-first/actual-first/completion-verification.json)、[外层显存/退出记录](../runtime/goal-baselines/20261106/final-asr-integration-first/actual-first-outer/exit.json)

## 验证与剩余边界

完整 workspace 测试 **894 passed、0 failed、14 ignored**；其中真实 GPU 诊断另行显式执行，**1 passed**。Python fake 测试 **27 项通过**；安装器的依赖保留、五文件部署和失败回退测试通过，后者只使用临时文件和 fake 进程。编译确有 **1 项 warning**：诊断源码 `stage` 赋值未读取，日志含该 warning 及汇总两行；不能写成零 warning。[workspace 汇总](../runtime/goal-baselines/20261106/final-asr-integration-first/binaries/manifest.json)、[实际单项诊断日志](../runtime/goal-baselines/20261106/final-asr-integration-first/actual-first-outer/diagnostic.log)、[Python 日志](../runtime/goal-baselines/20261104/final-asr-python-first.log)、[安装回退日志](../runtime/goal-baselines/20261104/final-asr-install-second.log)

ASR owner 当前与 TTS 使用同一 GPU 租约，策略是 **exclusive**。停止播放不等于卸载 TTS；本轮未启动 TTS，也未验证同卡共驻。实际麦克风/扬声器、摄像头联合负载、儿童/年龄分层、噪声和真实 GPU 推理期间取消仍未验收；正常 Stop 清理不能替代这些结论。原有 <450ms 或低 RMS 的录音门槛也仍在。

原生全部 bin 和 WebAssembly release 已构建成功，无生产构建 warning；耗时分别 169.82 秒和 23.38 秒。隔离主程序的 130 轮 HTTP 回归通过，额外验证 `final_asr` 状态输出、无配置不启动、缺失 managed 配置被拒绝、停用/重试接口及输入字段检查。该回归没有启用模型，不替代真实 Workstation 的 GPU 准备生命周期测试。

已归档 [27 文件候选增量](../runtime/goal-releases/20261106-final-asr/manifest.json)，共 27,014,094 bytes，带新 UI 和五文件终稿服务；主程序 SHA256 `b2ad2fc84cfb2eea9cb2f027e386b3d047cda56050c3acf229cc4f910c1d3520`，回退引用第 26 检查点。候选不含权重或环境，不是独立安装包；没有替换当前安装版、写入启用 policy 或发布。[应用回归](../runtime/goal-baselines/20261106/final-asr-integration-first/release-smoke-first/report.json)

`asr_ms` 现在累计本次 Stop 内部的捕获收尾、Qwen 等待和后续回退时间，另存 `asr_backend_ms` 子阶段，避免 GPU 超时后只显示 CPU 阶段而低报总延迟。历史记录保留原口径。GPU-only 已就绪 client 也计入 `asr_ready`，不会因缺少 CPU 权重而被自动轮换的就绪门挡住。

下一步验证真实 Workstation 启动、准备/取消/停止、实际 CPU 回退及 GPU 共驻，随后进入真实麦克风与联合负载；本轮仍未通过浏览器渲染验收。完整 Goal 继续 ACTIVE，实体 PTZ 继续遵守用户暂缓安排。
