# Microsoft WinPixEventRuntime

本目录包含 Microsoft 官方 `WinPixEventRuntime` NuGet 包中的 x64 Desktop DLL，供 DX12 命令列表写入 PIX CPU/GPU 命名事件。

- 包：`WinPixEventRuntime`
- 版本：`1.0.240308001`
- 来源：<https://www.nuget.org/packages/WinPixEventRuntime/1.0.240308001>
- DLL：`bin/x64/WinPixEventRuntime.dll`
- SHA-256：`81ADCFD8253C3489BE720DA7E30F16004DC9A1F02A8B418C6C3AEF4993032E6D`
- 许可证：MIT，见 `LICENSE.txt`
- 第三方声明：见 `ThirdPartyNotices.txt`

`build.rs` 会把 DLL 和许可证文件复制到当前 Cargo profile 输出目录，使它们与 `ray_tracing_demo.exe` 相邻。运行时只从可执行文件目录加载该 DLL，避免依赖全局 `PATH` 或当前工作目录。

该 DLL 只负责 PIX instrumentation。删除或加载失败不会改变渲染结果，但 PIX Capture 中不会显示本项目写入的 AS、Path Trace、Temporal、À-Trous 和 ToneMap 命名区间；benchmark JSON 会把 `pix_events_available` 报告为 `false`。
