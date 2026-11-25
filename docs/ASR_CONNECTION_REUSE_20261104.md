# ASR 连续流连接故障与修复记录

后续第26检查点已通过固定1315条完整推理及12条实时回放，见 [多连接与完整ASCEND记录](ASR_MULTICONNECTION_AND_ASCEND_20261104.md)。下面保留第25检查点当时的故障、修复尝试和未完成结论，不重判旧实验。

第25检查点明确捕获连接阶段的Windows `10048 / AddrInUse`，并实现同一次录音内复用HTTP连接。新的完整语料实验仍在下一次start建连时超时，尚未解决全部连续运行故障。原完整语料、两轮诊断和修复后实验分别保留，不用后续成功补齐旧准确率。

## 已取得的实际证据

| 运行 | 声明条数 | 实际尝试 | 终稿 | 失败位置 | 未运行 |
|---|---:|---:|---:|---|---:|
| 原完整语料 | 1315 | 363 | 362 | ID00362 / chunk sequence112 | 952 |
| 独立prefix v1 | 363 | 115 | 114 | ID00114 / sequence55 | 248 |
| 独立prefix v2 | 363 | 144 | 143 | ID00143 / sequence218 | 219 |

两次prefix均固定原ordinal0–362、每条最多一次、首失败停止、单个自有服务、600秒阶段上限，没有重试或替换输入。v1保留原生产`response/validate`的有界错误类别，定位至`request.send()`返回错误；v2增加可选的数值观测，仍共用原生产response实现，默认调用不保留这些观测。超时、重试、取消、身份及修订校验策略未改变。

v2失败包含：`is_connect=true`、`is_timeout=false`、`is_request=true`、`http_status=null`，三层source链内的I/O类别为`AddrInUse`、系统错误码10048。该块尝试发送640字节，此前218块已确认；未收到该块的有效响应。Microsoft将10048定义为地址已在使用，可能涉及现有或尚未关闭的socket，也可能在connect时才报告。**这证明本次地址占用错误，不单独证明全局动态端口池完全耗尽；此前两次未记录底层码的失败也不能追认成同一原因。** [Microsoft Winsock说明](https://learn.microsoft.com/en-us/windows/win32/winsock/windows-sockets-error-codes-2)。

旧Python服务使用HTTP/1.0，并为每个20ms分块关闭连接。这条代码路径在连续回放中大量新建短连接。已改为成功start/chunk保持当前连接、final/cancel/错误/health关闭，继续单线程识别和原有认证/实例/序号约束；未扩大全局端口范围、修改系统注册表或自动重试音频。

两次prefix的取消请求均收到HTTP成功状态，但未校验完整取消回执；真正清理证据是精确owned服务的model release、child/interpreter退出、两Job为空、`cleanup_confirmed=true`和`forced=false`。安装exe与旧描述文件保持原哈希，未使用18867旧服务。v1的114条与v2的143条已完成结果，均与原完整运行中相同ID的PCM哈希及原始hypothesis逐条一致；这个局部一致性不补全失败、不证明全量准确率。

## 软件验证和边界

v1定向5项通过，包含真实AsrClient访问fake HTTP服务的错误实例、修订号、409和无效JSON。v2流式ASR范围16项通过、4项真实模型默认忽略。首次新增闭端口测试误断言`is_timeout=false`，实际Windows先触发既有500ms连接时限；保留原15通过/1失败日志，修正此平台假设后通过。仍要求connect分类、原用户错误和不保留私密URL/token/body，未改生产策略。

数值观测最多遍历8层错误链，仅保存reqwest类别布尔值、HTTP状态、I/O枚举名和系统码，不保留原始error/source字符串、URL、header、token或请求/响应正文。

连接复用已实际落地。HTTP/1.1及TCP_NODELAY避免每块新建连接及头/body分次写入等待；RawIOBase读取适配器保留BufferedReader和原3秒底层读预算，每次select最多50ms检查停止标志。主线程只设flag，accepted socket由serving线程关闭。

第一版直接SHUT_RDWR在本机未唤醒带timeout的读取：25个测试方法中，owner stop/EOF两子例报错；栈和独立探针确认约3秒后才退出。另一个detach/close探针虽快速退出，但该候选未进入验证或生产：Microsoft禁止同一socket上的closesocket与其他Winsock调用并发，且handle可能被立即复用。最终选择协作读取，没有采用跨线程强关handle。[Microsoft关闭socket约束](https://learn.microsoft.com/en-us/windows/win32/api/winsock/nf-winsock-closesocket)。

最终提议28项fake测试通过；root应用后完整Python发现31项通过（含Windows pipe测试），Rust流式范围16项通过。覆盖同连接多块、final/cancel/错误/health关闭、HTTP pipeline、部分body中止、原读预算以及两个accept/stop交错。fake owner stop/EOF约62/59ms，不代表真实模型推理或声学停止时延。

## 修复后新实验仍未完整通过

冻结新输出`ascend-zipformer-reuse-first`及独立1315+12预算。完整阶段124次尝试、123条终稿（16空文本）；ID00123在start、尚未发送任何PCM时，触发约508ms请求失败。记录为connect=true、timeout=true、I/O TimedOut、系统码null，对应现有500ms connect deadline。剩余1191条未运行，全部1315条状态保留。不能把这次TimedOut写成再次观察到10048，也不能宣称已完成完整语料准确率或故障修复。

预定实时子集12/12完成，原始hypothesis及PCM哈希与第24检查点12条逐条一致。EOF到终稿中位23.049ms、样本最大24.243ms；3181块的请求到有效响应中位0.801ms、样本P95为11.732ms。小样本、共享宿主机、非随机配对，不据此推导总体性能提升比例；仍不覆盖麦克风、VAD、UI或声学延迟。两个新服务均正常清理、无forced，保护文件哈希不变。

下一片评估成功final后跨录音保活，并用真实Rust AsrClient加真实Python Handler/fake engine核对连接复用；仅Python HTTPConnection测试不足以证明reqwest路径。需要保留每段独立decoder、UUID/generation隔离、闲置超时和owner stop。当前不放宽500ms连接时限、不添加请求重试，原失败保持原判。

证据目录：`ascend-failure-prefix-first`、`ascend-failure-prefix-second`、`ascend-zipformer-reuse-first`、`checkpoint25-failure-first-*`、`checkpoint25-send-first-sources`、`checkpoint25-send-second-*`、`checkpoint25-reuse-first-*`；另有独立静态/运行审阅JSON。都位于 `runtime/goal-baselines/20261104`。原生生产release和安装版未替换。
