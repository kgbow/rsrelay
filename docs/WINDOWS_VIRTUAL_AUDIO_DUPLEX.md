# 跨平台（Windows + macOS）全双工播放器与虚拟麦克风共享架构说明

本文档记录了 `rsrelay` 针对 **跨机器（Windows + macOS）同时共享播放器与麦克风** 的完整根因诊断、底层虚拟声卡驱动协议、正交双总线路由架构及已落地的三步工程方案。

---

## 1. 核心根因总结

1. **Windows 虚拟麦克风喂数饥饿（15.625 ms 系统时钟粒度陷阱）**：
   - 早期实现使用 `thread::sleep(5ms)` 且超时不做补帧，在 Windows 10/11 默认 `15.625 ms` 时钟粒度下实际每 ~15.6 ms 仅向 `\\.\AudioRelayVirtualMicDeviceLink` 写入 5 ms（`960` 字节）数据，仅达到 48 kHz 标称速率的 **~32%**，导致内核录音环形缓冲长期欠载断音。
   - **修复方案**：改用 Windows 原生高精度等待定时器 `CreateWaitableTimerExW(..., CREATE_WAITABLE_TIMER_HIGH_RESOLUTION)`（亚毫秒级唤醒），结合墙钟流逝帧数补帧（Catch-up）与驱动返回的 `[capacity_bytes, queued_bytes]` 实时水位闭环控制（将内核缓冲稳定锁定在 `10~45 ms` 区间），实测 48 kHz 立体声吞吐率达 **100%**。

2. **无虚拟声卡环境下的外放环回自激（Echo-Free Guard 防护）**：
   - 当某台电脑（如未装驱动的 Mac 或纯净 Windows）开启「接收对端麦克风」时，若退化回系统默认扬声器，对端语音会直接外放并被本机 `SystemLoopback` 重新录回造成 100% 回声。
   - **修复方案**：新增 [`NullVirtualMicStream`](file:///c:/Users/kevin/Documents/code/ai/rsrelay/crates/rsrelay-audio/src/windows_virtual_mic.rs#L48-L128) 防回声保护接收器——未检测到虚拟麦克风驱动时，按 48 kHz 实时消费网络语音并驱动界面 VU 电平表，**绝不向物理扬声器输出任何声音**，同时在界面弹出 **一键安装虚拟麦克风** 引导。
   - 同时在自动选择物理麦克风（[`capture.rs`](file:///c:/Users/kevin/Documents/code/ai/rsrelay/crates/rsrelay-audio/src/capture.rs#L312-L343)）时自动跳过虚拟声卡输入端点（`Virtual Mic`、`CABLE Output`、`rsrelay Virtual Mic`、`BlackHole`），防止用户将系统默认录音设备设为虚拟麦后产生跨网死循环。

3. **正交双总线解耦架构（支持 Win ⇄ Mac 任意组合共享播放器与麦克风）**：
   - 在 [`duplex.rs`](file:///c:/Users/kevin/Documents/code/ai/rsrelay/crates/rsrelay-core/src/duplex.rs) 中，将音频流解耦为两条物理端口隔离、零串扰的独立总线：
     - **播放器总线（Media Bus `:49153`）**：`SystemLoopback`（Win WASAPI 内录 / Mac `ScreenCaptureKit` 免驱内录）⇄ 对端物理耳机/扬声器。支持 `关闭 | 发内录 | 收听 | 双向`。
     - **麦克风总线（Mic Bus `:49155`）**：物理麦克风（含噪声门、DTX 省流与静音开关）⇄ **仅注入对端虚拟麦克风**。支持 `关闭 | 发本机麦 | 进虚拟麦 | 双向`。
   - 顶部角色预设支持 **`主机电脑 (Host)`**、**`无线耳麦 (Headset)`** 与 **`双机互享 (Symmetric)`**，同时支持在左右两张卡片内独立定制任意流向（共 `4 × 4 = 16` 种跨机路由组合）。

---

## 2. Windows 与 macOS 虚拟麦克风多后端体系

### 2.1 macOS 原生专属 HAL 驱动：`rsrelayVirtualMic.driver`
- **源码位置**：
  - [`drivers/macos/rsrelay_virtual_mic.c`](file:///c:/Users/kevin/Documents/code/ai/rsrelay/drivers/macos/rsrelay_virtual_mic.c)
  - [`drivers/macos/Info.plist`](file:///c:/Users/kevin/Documents/code/ai/rsrelay/drivers/macos/Info.plist)
  - 一键编译签名安装模块：[`crates/rsrelay-audio/src/macos_virtual_mic.rs`](file:///c:/Users/kevin/Documents/code/ai/rsrelay/crates/rsrelay-audio/src/macos_virtual_mic.rs)
- **核心设计优势（对比单设备 Loopback 与外部 `shm_open` IPC）**：
  1. 在同一个用户态 CoreAudio `AudioServerPlugIn`（`/Library/Audio/Plug-Ins/HAL/rsrelayVirtualMic.driver`）内暴露两个分离端点：
     - **`rsrelay Virtual Mic`**：**纯输入（Capture-only）麦克风端点**（0 输出流），直接显示在 Zoom、Chrome、微信、飞书、Discord、OBS 的麦克风列表中，**绝不污染扬声器列表**。
     - **`rsrelay Virtual Mic Sink`**：专用内部桥接输出端点，设置 `CanBeDefaultDevice = 0` 且 `CanBeDefaultSystemDevice = 0`，**macOS 绝不会在安装或拔插耳机时误将系统默认扬声器切走**。
  2. 两个端点在 `coreaudiod` / `DriverHelper` 同一进程地址空间内通过无锁原子环形缓冲（`gBridgeRing`）直通，**完全规避 macOS Sonoma / Sequoia Seatbelt 沙盒对跨进程 `shm_open` / Mach Port 的拦截**，且由 CoreAudio `mach_absolute_time` 硬件时钟精确驱动。
  3. 同时天然兼容已安装 `BlackHole 2ch` 或 `VB-CABLE` 的 macOS 环境。

### 2.2 Windows 多后端优先级策略
在 [`select_default_virtual_mic_output_device()`](file:///c:/Users/kevin/Documents/code/ai/rsrelay/crates/rsrelay-audio/src/device.rs#L139-L176) 中按以下最佳稳定性优先级自动绑定：
1. **`VB-CABLE` (`CABLE Input`)**：由 Windows `audiodg.exe` 硬件中断回调拉取数据，且不与后台运行的 `AudioRelay` 产生句柄竞争（支持通过界面按钮或 `rsrelay install-driver` 调用 `winget install -e --id VBAudio.VBCable` 一键安装微软 WHQL 签名驱动）。
2. **`Virtual Mic (Virtual Mic for AudioRelay)`**：直接通过高精度等待定时器 + 水位闭环驱动 `\\.\AudioRelayVirtualMicDeviceLink`（若检测到后台运行 `audiorelay-backend.exe`，GUI 自动给出防抢占提示）。
3. **其他虚拟音频线**：自动识别 `Virtual Audio Driver`、`VoiceMeeter Input` 等设备。

---

## 3. 快速使用指南（Win + Mac 跨机共享）

1. **安装虚拟麦克风（仅在需要“把别的机器麦克风当本机麦克风用”的那台机器上执行一次）**：
   - **macOS**：在 `rsrelay` 的「双工耳麦」或「音频设备」页点击 **`一键安装虚拟麦克风`**（或终端执行 `cargo run -p rsrelay-cli -- install-driver`），授权管理员密码后即可在系统麦克风列表看到 `rsrelay Virtual Mic`。
   - **Windows**：若已装过 AudioRelay 虚拟声卡驱动则直接开箱即用；纯净新机点击 **`安装 VB-CABLE 驱动`**（或运行 `rsrelay install-driver`）。
2. **常用跨机模式**：
   - **两台电脑互享播放器 + 互享麦克风**：两台机器均切到「双工耳麦」页，角色均选 **`双机互享`**，填入对方局域网 IP 后点击「开始通话」。
   - **单向把 Mac 麦克风共享给 Win 开会/打游戏（或把 Win 麦克风共享给 Mac）**：发送方在右侧麦克风卡片选 **`发本机麦`**，接收方选 **`进虚拟麦`**，在微信/浏览器/Zoom 中将麦克风选为界面提示的虚拟麦克风名称即可。
