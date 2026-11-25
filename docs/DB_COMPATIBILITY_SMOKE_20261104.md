# 新版 → 旧版 → 新版数据库兼容性 smoke

根代理已审核并实际运行此脚本，三个阶段全部通过且均正常退出。作者随后独立读回报告和三个语义/模式快照、重算语义哈希并逐值比较。没有编译、改服务源码或迁移逻辑，也没有读取真实数据库或凭证。

入口为 `scripts/validation/database_compatibility_smoke.py`。两个二进制固定为：

| 角色 | 路径 | SHA-256 |
| --- | --- | --- |
| 新版，566 项测试冻结检查点 | `runtime/goal-releases/20261104-seventh/bin/ai-tutor.exe` | `99f7efefadecb1c51cd6d406a3deea833236d47791b7cdcac71d64bf74112764` |
| 当前安装的旧版，根代理标记为 261 项版本 | `bin/ai-tutor.exe` | `16c654e1dae5fbce5431ce1df3a5b3616da655e7106ef7bb011bf5ceefbcaf1d` |

执行前校验哈希，输出目录必须是 `runtime/goal-baselines/20261104` 下尚不存在的新目录。脚本创建独立 fixture root，不复制生产 runtime、数据库、模型、prompts 或服务描述符。它只写自身的配置及合成输出文件；业务数据通过程序的 HTTP API 写入，不使用 SQL 补写或修复数据库。

三个阶段使用同一 fixture root、不同自选 loopback 端口，隐藏启动且传 `--no-browser`。环境只传操作系统启动路径和 fixture 内新建的 HOME/USERPROFILE、空 Kimi home、空 PATH、AppData、TEMP，拒绝继承真实模型凭证或外部服务配置。fixture 显式设置 `local-speech-policy.json` 为 disabled、warmup disabled。每次 ready 与阶段结束前确认 camera closed、audio idle/recording false、PTZ off、Kimi unavailable/requests 0、如有 local_speech 则 disabled。没有调用摄像头、麦克风、播放、PTZ、模型或教学生成接口。

1. 新版创建一个模拟档案及会话，保存一条新格式比较。A/B 均 `not_tested`，结果和次数均 null，条件 `not_confirmed`，待执行计划 `accepted=false`。确认比较读回、未产生观察或接受的步骤、慢记忆为空，正常关闭。
2. 旧版打开同一个 fixture 数据库，通过双方共有的 `GET /api/state` 读取原档案，通过 `POST /api/learners` 新增另一个模拟档案并读回，正常关闭。没有用旧版不支持的新比较 API，也没有要求它理解新功能。
3. 新版再次打开，读到两个版本写入的档案、恢复原会话、读取完全相同的新比较和计划；确认计划仍未接受/执行/完成，没有观察或慢记忆，正常关闭。

每阶段程序退出后，仅对 fixture 的 simulator DB 建立 SQLite `mode=ro` 连接，做 `integrity_check` 并归档原档案、任务 state、比较 receipt、两个 plan 原始 event、类型/版本元数据及空 memories 的快照。语义字段三阶段必须逐值相同；generation/status 另行记录，不把正常关闭时的运行代次变化当作业务丢失。schema SQL 及其哈希也逐阶段归档，报告是否变化；不修正或掩盖旧版不兼容。

进程所有权通过子进程句柄及 fixture descriptor 的 PID/port 精确绑定；随机 token 只在内存用于自有 HTTP。每次正常 `/api/shutdown` 后等待最多 15 秒，超时只终止自己创建的 Popen child，记录 forced stop 并使验证失败。不按外部 PID、端口或发行版停止进程。进程退出后只清除仍绑定自己 PID/port/token 的 fixture descriptor，报告及快照不保存 token。

根代理审核后的命令：

```powershell
python scripts/validation/database_compatibility_smoke.py --local-synthetic --root . --output runtime/goal-baselines/20261104/database-compatibility-first
```

预期产物为 `report.json`、`after-new-create.json`、`after-old-write.json`、`after-new-reopen.json` 和保留的合成 fixture。任何失败保留实际阶段和错误分类，不修改数据库重试成通过。这个 smoke 仅验证本 fixture 的启动/迁移、稳定档案 API 写入和新比较/计划数据保持；即便通过也不等于旧版所有删除、更正、教学或新功能操作都支持回退。

## 首次实际证据

产物在 `runtime/goal-baselines/20261104/database-compatibility-first/`，完成时间为北京时间 2026-11-04 04:33:02。原 `report.json` 的 SHA-256 为 `7c5cc95a647bb2491ad2d53b227e21c75acdab7c7689bcca060184852dc8e945`，报告及二进制未改写。

| 阶段 | 业务核对 | 正常 shutdown / exit | SQLite integrity |
| --- | --- | --- | --- |
| 新版创建 | 模拟档案、原会话及新比较读回；两个未接受、未执行的 plan 来源 | accepted / 0 | ok |
| 旧版读写 | 读取新版档案；通过稳定 API 新增旧版档案并读回 | accepted / 0 | ok |
| 新版复核 | 两个档案均可读；原会话、比较与计划保持；无慢记忆 | accepted / 0 | ok |

三个快照的业务语义逐值相同，重算 SHA-256 都为 `65286fe88c9da8eb971201254be2adcbf667d87b90585a77bd17de03bf3ce6dd`；完整 schema SQL 也逐值相同，哈希均为 `49c5608567935c3245b2a15cb34e14db44a229c7c3d3c250844940640c8f29c7`。两个来源始终为 active、version 1、kind plan、accepted false；A/B 结果及次数仍为空。任务 stage 保持 goal_setting，reported_result 和 pending_step 为空、attempts 为零、memories 为零。运行 generation 是 2 → 2 → 3，最后一次恢复/关闭造成运行代次推进，没有改业务语义。

三个子进程都由 HTTP 正常关闭并回收，未强制终止；状态检查均为设备关闭、模型请求零，新版 local_speech disabled。fixture server descriptor 已消失；重新核对新旧二进制哈希与执行前一致。

通过范围仅限上述固定 fixture 和三个阶段。没有测试旧版编辑、删除、更正新来源或新比较，也没有测试流式交付、语音账本、所有历史迁移组合、并发读写、备份恢复或真实数据库。因此该结果支持“这条既定升级/回退/再升级路径保持数据”，不能宣传“任意降级操作完全兼容”。
