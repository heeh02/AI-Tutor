# AI Tutor 后端 API 交接

本文面向并行界面线程。界面只调用现有 Rust 本机服务，不直接读写 SQLite，不启动 Kimi 子进程，不修改 ACP agent、凭证目录或模型拥有者。权威实现为 `rust/ai-tutor/src/server.rs`；命令行封装见 `docs/CLI.md`。

## 1. 启动与首选入口

```powershell
.\bin\ai-tutor.exe --root . --no-browser
.\bin\tutorctl.exe --root . status
.\bin\tutorctl.exe --root . status --full
.\bin\ai-tutor.exe --stop --root .
```

优先使用 `tutorctl` 完成档案、活动、教学文字和记忆操作。它已经处理本机认证、请求格式、超时、对象匹配和凭证脱敏：

```powershell
.\bin\tutorctl.exe --root . learners list
.\bin\tutorctl.exe --root . learners create --name '界面模拟档案' --namespace simulator
.\bin\tutorctl.exe --root . session select --learner learner_ID
.\bin\tutorctl.exe --root . activity report --source simulated --modules control,motor,distance
.\bin\tutorctl.exe --root . turn send --text '我想做一个测距实验' --wait --wait-seconds 120
.\bin\tutorctl.exe --root . memory list --learner learner_ID
.\bin\tutorctl.exe --root . session finish
```

`turn send --wait` 只观察生成结果，不确认页面已经显示，也不启动朗读。分句生成中返回前会等待 `tutor_stream.phase` 离开 `generating`；`final_pending` 表示已有完整结果但仍待显示，`completed` 才表示显示流程完成。失败、取消或中断返回非零退出，已显示前缀不等于整轮成功。界面必须在真实可见后调用显示确认接口；否则教学与记忆候选不会按已投递处理。

## 2. 原始 HTTP 的认证约定

- 地址只能使用 `http://127.0.0.1:{port}`。端口和一次性启动凭证位于 `runtime/server.json`；不要记录、转发或持久化凭证。
- 浏览器先打开 `GET /?token={token}`。服务通过 303 跳转到 `/`，并设置 `ai_tutor_local` 的 `HttpOnly; SameSite=Strict; Path=/` Cookie。
- 非浏览器客户端可复用 `tutorctl`，或自行从描述文件读取端口和凭证，并发送 `Cookie: ai_tutor_local={token}`。不要把凭证放进 URL 参数、业务 JSON 或日志。
- `GET /api/health` 无需 Cookie，仅用于本机存活检查。
- `Host` 必须是 `127.0.0.1:{port}` 或 `localhost:{port}`。带 `Origin` 时必须精确等于 `http://{host}`；`Sec-Fetch-Site: cross-site` 会被拒绝。WebSocket 使用同样的认证与来源检查。
- 请求体为 UTF-8 JSON。未知字段会被拒绝，不要透传额外执行字段。
- 成功响应是业务 JSON 本身，没有统一成功包装。业务错误通常为 HTTP 400：`{"detail":"可读错误信息"}`；认证失败为 401，来源检查失败为 403。

## 3. 核心端点

| 方法与路径 | 请求与用途 |
|---|---|
| `GET /api/state` | 完整工作台状态：档案、当前会话、教学提供方、音频、语义检索、指标和有界错误列表 |
| `POST /api/learners` | `{"name":"匿名名","namespace":"simulator","language":"zh","help_style":"guided"}`；`namespace` 为 `real` 或 `simulator` |
| `POST /api/sessions` | `{"learner_id":"...","resume":true}`；只在这里切换当前档案/恢复活动 |
| `GET /api/learners/{lid}/sessions` | 活动目录；界面另用续做接口，不能通过查看历史改变当前活动 |
| `POST /api/learners/{lid}/sessions/{source_sid}/continue` | 显式续做已结束活动；使用响应中的新会话身份，旧当前事实不会自动复制 |
| `POST /api/sessions/{sid}/turns` | `{"text":"...","auto_speak":false,"client_submission_id":"唯一提交号","expected_generation":当前代次}`；返回 `request_id` 与新 `generation` |
| `GET /api/sessions/{sid}/streams/{rid}` | 读取指定教学流，不能用另一轮的全局“最近状态”代替 |
| `POST /api/sessions/{sid}/streams/{rid}/display` | `{"generation":当前代次,"sequence":0}`；仅在对应文字实际可见后调用，重复确认须保持幂等 |
| `POST /api/sessions/{sid}/delivery` | 整段模式：`{"turn_id":"...","status":"shown"}`；同样只能在真实显示后调用 |
| `POST /api/sessions/{sid}/cancel` | 取消当前生成和旧播报，不等于结束活动 |
| `POST /api/sessions/{sid}/finish` | 保存并结束当前活动，保留有来源的进展 |
| `GET /api/sessions/{sid}/context` | 当前快记忆与召回条目，仅供明确检查 |
| `POST /api/sessions/{sid}/settings` | 可改 `language`、`help_style`、`reading_level`、`speech_speed`；只传需要修改的字段 |
| `POST /api/activities/{aid}/events` | `{"modules":["control","motor"],"source_type":"simulated","report":"..."}`；不传 `modules` 就不改变模块；模拟来源只能写入模拟空间 |
| `GET /api/learners/{lid}/memories` | 当前有效慢记忆列表 |
| `GET /api/learners/{lid}/memories/{mid}/evidence` | 来源原文、版本、活动关系和比较背景 |
| `PATCH /api/learners/{lid}/memories/{mid}` | `{"content":"明确更正后的内容"}` |
| `DELETE /api/learners/{lid}/memories/{mid}` | 删除一条记忆及其来源派生范围；不会把独立实验和续做选择一起广删 |
| `DELETE /api/learners/{lid}/data` | 显式清空该档案；这是破坏性操作，界面必须单独确认 |
| `POST /api/learners/{lid}/sessions/{sid}/help-preference` | 保存明确、持久的讲解偏好；使用服务返回的提交号和代次做幂等重试 |
| `GET/POST /api/learners/{lid}/sessions/{sid}/comparisons` | 读取或保存结构化 A/B 实验；使用 `submission_id` 防止重复写入 |
| `GET/POST /api/learners/{lid}/sessions/{sid}/memory-review` | 获取或显式发起当前活动的慢记忆整理 |
| `POST .../memory-review/candidates/{candidate}` | `{"decision":"retain"}` 或 `{"decision":"skip"}`，以实际允许值为准；候选不能绕过来源与类别门禁 |
| `POST /api/local-memory/retry`、`POST /api/local-memory/stop` | 空 JSON `{}`；显式重试或停止可选的本地 BGE 检索 |
| `GET /api/events`（WebSocket） | 接收状态变更通知；收到通知后重新读取对应权威状态，不把通知当作业务提交回执 |

设备、录音、语音轮换和 PTZ 沿用 `docs/CLI.md` 中的约束。它们不是普通页面初始化动作，不得自动开启。特别是 PTZ 继续保持 `off`，不要自行构造启用请求。

## 4. 界面必须保留的教学边界

1. 从响应取得 `session_id`、`request_id`、`generation`，后续显示确认和流读取都绑定这些身份。
2. `generated` 不等于 `shown`。模型文本只有实际可见后才确认；失败、切换档案、遗忘或新输入可能使旧确认失效。
3. 分句模式逐段确认 `sequence`；整段模式确认 `turn_id`。不要同时调用两套接口。
4. `auto_speak:false` 只控制自动朗读，不影响文本生成。默认交接验证使用纯文字，避免意外发声。
5. `tutor-delivery.json` 与教学提供方策略在启动时固定。界面读取状态，不在请求之间改写配置。
6. 教师建议不是执行完成，模拟来源不是硬件验证。界面不能添加 `shell`、`actuator`、电机执行或硬件可信升级字段。

## 5. 状态与后端诊断

`GET /api/state` 中：

- `teaching.provider`：当前提供方，例如 `kimi` 或 `deepseek_flash`。
- `teaching.available`：本地配置与既有核验是否允许发起请求，不是远程模型已成功回复的证明。
- `kimi.metrics.last_acp_segments`：最近一次 ACP 调用的有界诊断。必须按 `request_id` 和 `generation` 匹配，不能把它当作“最大代次”。
- `failure_phase`：`prepare`、`queue`、`spawn`、`initialize`、`session_new`、`profile`、`prompt_write`、`prompt_response`、`segment_validation`、`final_validation`、`session_close` 或 `cleanup`。
- `failure_code`：固定机器码。`acp_profile_not_verified` 表示未取得本回合预期的隔离教学 profile，不应引导用户反复登录。
- `prompt_write_attempted/completed`：本地管道写入状态，不是远端接受或开始生成的回执。
- `process_reaped`、`owned_directory_removed`、`stderr_drained`：本地清理证据；`null` 表示尚未取得，不等于成功。
- `local_memory`：`phase`、当前档案 `coverage`、回退原因和清理状态。未完成时仍可使用词面检索，不应阻止普通对话。

原始提示词、远程错误正文、思考文本、凭证、系统路径和模型上下文不应进入界面诊断展示或日志。

## 6. 数据与发布约定

- 真实和模拟数据分别位于 `runtime/data/real.sqlite3` 与 `simulator.sqlite3`，只能经 API 访问。
- 页面、API、视频与 WebSocket 均只监听本机；不要把服务绑定到 `0.0.0.0` 或暴露到局域网。
- 安装更新必须同时包含 Rust 程序、CLI、完整前端资产，以及 `bin/local-memory/` 的 `managed_embedding.py`、`embedding_worker.py`。BGE 模型、WSL 环境和 `runtime/local-memory-policy.json` 是保留的本地依赖，不进入程序备份。
- 可选 BGE 默认关闭。配置和冷启动说明见根目录 `README.md`；不要在首次页面加载时隐式下载数 GB 模型。
- 完整构建回归命令见 `README.md` 与 `ACCEPTANCE_REPORT.md`。历史设备验收不能被当前软件测试替代。
