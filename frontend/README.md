# Rust WebAssembly 工作台

所有界面、网络请求、按键语音、投递确认、记忆操作和过期结果保护均位于 `rust/tutor-ui/src`，使用Rust/Yew并编译为WebAssembly。

运行资产：`frontend/dist`。HTML/CSS及wasm-bindgen自动生成的浏览器桥接文件由本机Rust服务同源提供。没有React、TypeScript、Node或npm运行依赖。

构建：`powershell -NoProfile -ExecutionPolicy Bypass -File frontend/build.ps1`。详情与官方wasm-bindgen工具来源见`rust/tutor-ui/README.md`。

旧版前端仅作为历史参考保存于`archive/pre-rust-reference/frontend`，不会被构建或提供给浏览器。
