# Rust 原生子进程 I/O 与超时合同验收

日期：2026-11-01，Asia/Shanghai。范围：自动合同测试，不调用真实 Kimi、认证、摄像头、
麦克风、语音、学习者或活动接口；没有修改全局配置或运行中的服务。

测试使用独立 Rust `io-fixture` 开发辅助程序和真实 Windows 匿名管道。辅助程序在收到
单字节启动门标记前不输出数据；父进程先将其加入私有 Job Object，再释放启动门。
测试复用生产代码的 `KimiProvider::bounded`、Job Object 和应用自有目录清理实现。
所有目录与“共享凭证”目标均为临时合成夹具，不是用户的真实凭证。

| 对应项 | 已执行合同 | 结果 |
|---|---|---|
| D06 并发排水、非零退出 | 原生子进程同时输出 512KiB stderr 和 32KiB stdout；父进程并发排空，逐字节内容一致，明确得到退出码 7，无死锁 | PASS |
| D06 UTF-8 分片 | 原生匿名管道接收端强制每次读取一个字节，中文与 emoji 跨读取分片；完整重组后解析 JSONL，得到精确文本“测距😀中文”；非法 UTF-8 被拒绝 | PASS |
| D06 输出上限 | 分别向 stdout、stderr 产生 512KiB 输出，使用生产 256KiB stdout / 128KiB stderr 上限，实际有界读取器报错；停止并回收所属子进程 | PASS |
| D08 超时与清理 | 合成进程先输出 READY，确认进入 5 秒睡眠分支；合同 deadline 设为 30ms，实际触发超时，回收所属进程，删除自有目录并保留共享合成目标 | PASS |

并发完整排水合同为验证大 stderr 的活性，将该单项测试的读取上限设为 768KiB；
双向超限测试另外使用真实生产上限。测试不会把大 stderr 当成可显示的教学文本。

这次执行 **4 passed，0 failed，0 ignored**。未执行真实供应商额度、断网或认证失效故障注入。
生产请求 deadline 仍为 90 秒；本次没有等待真实 Kimi 请求触发该 deadline。

## 实际执行命令

在工作区根目录执行：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts/build-rust.ps1 build -p ai-tutor --bin io-fixture
powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts/build-rust.ps1 test -p ai-tutor --lib io_contract_ -- --nocapture
```

此处 ExecutionPolicy 仅作用于本次开发工具进程，没有修改用户或系统执行策略。
生产应用没有新增 PowerShell/Python 调用。

执行的测试名称：

```text
kimi::io_contract_tests::io_contract_concurrent_pipe_drain_and_nonzero_exit
kimi::io_contract_tests::io_contract_fragmented_utf8_reassembles_before_json_decoding
kimi::io_contract_tests::io_contract_stdout_and_stderr_limits_stop_owned_children
kimi::io_contract_tests::io_contract_short_deadline_cleans_sleeping_child_and_directory
```

## 生产版本保持不变

本轮只添加 `#[cfg(test)]` 合同代码与独立开发辅助程序，未修改生产逻辑。
运行中的 `bin/ai-tutor.exe` 在执行前后的 SHA-256 一致：

```text
469960ec7c441cb69fc254215e126457b9d814547a7008fe95f9e434f80b990c
```

代码位置：`rust/ai-tutor/src/kimi.rs` 的 `io_contract_tests` 模块与
`rust/ai-tutor/src/bin/io-fixture.rs`。没有增加或重新执行远程付费请求。
