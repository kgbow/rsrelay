# `rsrelay` UI Design System — **Sonic Obsidian (声学黑曜石)**

`rsrelay-gui` 采用专为超低延迟专业音频传输与实时 DSP 遥测打造的 **Sonic Obsidian (声学黑曜石)** 设计风格。该设计系统融合了 **现代硬件音频工作站（DAW / 监听控制器）的精密声学仪表感** 与 **高密度网络/DSP 遥测控制台的清晰层级**，在保持 `< 8 MB` 极低内存开销与零外部贴图依赖（全矢量 `egui::Painter` 实时绘制）的前提下，提供现代精美的桌面级体验。

---

## 1. 设计理念与核心原则 (Design Philosophy)

1. **精密声学与低视觉疲劳 (Dark Studio Ergonomics)**
   采用多层级深邃黑曜石底色（`#0B0E14` -> `#121722` -> `#192030`）配合 `1px` 精密微边框（`#21293A`）与内凹仪表槽（`#0D111A`），消除纯黑背景的生硬眩光与默认灰底的廉价感。
2. **零抖动等宽遥测排版 (Zero-Jitter Monospace Telemetry)**
   所有高频刷新的实时指标（30Hz 刷新的 `dBFS` 电平、包计数器、Jitter 微秒数、`rubato` PI 锁相环重采样率 `1.00000x`、IP:Port 端点）统一使用等宽字体（`FontFamily::Monospace`），确保数值跳动时界面布局绝对稳定、不发生水平抖动。
3. **固定拓扑与零布局跳变 (Zero Layout Shift)**
   无论音频流处于 `STANDBY` 还是 `LIVE` 状态，声学电平表（VU Meter）、抖动缓冲水位波形图（Sparkline）与 Bento 遥测卡片始终驻留在固定位置，消除启停瞬间界面元素突然弹出/坍缩导致的视觉割裂。
4. **信号色彩语义化 (Semantic Signal Coding)**
   每条数据链路与声学区间均绑定唯一语义色：
   - **Acoustic Mint (`#00D4AA`)**：主品牌色、发射链路（TX）、正常声学动态区（`-60..-12 dBFS`）。
   - **Telemetry Cyan (`#38BDF8`)**：接收链路（RX）、mDNS 局域网雷达、Jitter Buffer 水位曲线与噪声门阈值线。
   - **Duplex Indigo (`#818CF8`)**：全双工无线耳麦模式（同时承载下行系统音频与上行麦克风）。
   - **Headroom Amber (`#F59E0B`)**：声学动态预警区（`-12..-3 dBFS`）、Opus LBRR FEC 前向纠错触发提醒。
   - **Clip Crimson (`#EF4444`)**：削波过载区（`-3..0 dBFS`）、丢包告警与紧急停止（Stop CTA）。

---

## 2. 色彩令牌体系 (Color Tokens)

定义于 [`crates/rsrelay-gui/src/theme.rs`](../crates/rsrelay-gui/src/theme.rs) 的 `palette` 模块：

| 语义层级 | 令牌名称 | Hex 色值 | 适用场景 |
| :--- | :--- | :--- | :--- |
| **Canvas (画布底色)** | `BG_CANVAS` | `#0B0E14` | 窗口根画布背景（Deep Obsidian） |
| **Surface (卡片表面)** | `BG_SURFACE` | `#121722` | 主功能机架卡片、顶部导航栏、底部状态栏 |
| **Elevated (悬浮交互)** | `BG_ELEVATED` | `#192030` | 按钮、下拉框、输入框默认态背景 |
| **Active (激活高亮)** | `BG_ACTIVE` | `#222B40` | 顶部模式切换器选中项、悬浮高亮背景 |
| **Trench (内凹槽位)** | `BG_TRENCH` | `#0D111A` | VU 电平表底槽、Sparkline 示波器底槽、Bento 指标卡 |
| **Border Subtle** | `BORDER_SUBTLE` | `#21293A` | 卡片与内凹模块的 `1px` 结构分割线 |
| **Border Widget** | `BORDER_WIDGET` | `#2E3A52` | 交互控件默认描边 |
| **Accent Mint** | `ACCENT_MINT` | `#00D4AA` | 发射器激活态、主操作按钮、健康电平段 |
| **Accent Cyan** | `ACCENT_CYAN` | `#38BDF8` | 接收器激活态、水位曲线、噪声门门限游标 |
| **Accent Indigo** | `ACCENT_INDIGO` | `#818CF8` | 全双工耳麦工作台主题色 |
| **Status Warn / Danger** | `STATUS_WARN` / `DANGER` | `#F59E0B` / `#EF4444` | `-12..-3 dB` 动态余量预警 / `> -3 dB` 削波与停止操作 |

---

## 3. 空间、圆角与排版规范 (Spatial & Typography Scale)

### 3.1 8px 基准空间网格 (`theme::metrics`)
- `SPACE_XS = 4.0px`：紧凑组件内间距、电平表标题与槽位间距
- `SPACE_SM = 8.0px`：标准控件行间距、Bento 卡片间距
- `SPACE_MD = 12.0px`：表单列间距、内凹面板内边距
- `SPACE_LG = 16.0px`：主工作室卡片内边距（Inner Margin）
- `RADIUS_CARD = 10.0px`：工作室大卡片圆角
- `RADIUS_WIDGET = 6.0px`：按钮、输入框、延迟配置卡圆角
- `RADIUS_PILL = 14.0px`：实时状态胶囊徽章（Status Pill）与局域网节点芯片圆角

### 3.2 排版层级
- **应用主标题 (`16.5px Bold`)**：搭配 `STUDIO DSP` 等宽徽章与矢量声波徽标。
- **机架模块编号标题 (`13.5px Monospace Bold`)**：如 `01 // TRANSMITTER (SENDER)`、`02 // RECEIVER (LISTENER)`，强化专业硬件机架辨识度。
- **Bento 遥测数值 (`13.5px Monospace Bold`)**：主指标大字号等宽显示，辅以 `10.5px` 次级指标说明。

---

## 4. 核心自定义组件库 (`crates/rsrelay-gui/src/widgets.rs`)

1. **对数 dBFS 专业分段式 VU 电平表 (`draw_vu_meter` + `VuMeterState`)**
   - 将线性振幅 `[0.0, 1.0]` 映射为对数 `[-60.0, 0.0] dBFS`（`20 * log10(peak)`）。
   - 内置快攻慢放（Fast Attack / Smooth Exponential Release，`38 dB/s`）与峰值保持（`650ms` Peak Hold + `22 dB/s` 衰减）物理阻尼特性。
   - 36 段高对比度 LED 矩阵分段渲染，未点亮段保留分区微暗底色；支持在全双工模式下实时绘制 **Cyan 噪声门门限游标（Noise Gate Threshold Line）**。
2. **实时抖动缓冲水位示波器 (`draw_sparkline` + `SparklineBuffer`)**
   - 维护 64 帧滚动历史队列，实时绘制 `JitterBuffer` 平滑水位（`ms`）曲线、半透明垂直积分柱以及 `Target Buffer (ms)` 参考基准线。
3. **4 档延迟与编码配置矩阵卡 (`latency_profile_selector`)**
   - 将传统的下拉菜单升级为平铺式四联选卡（`Ultra-Low` / `Balanced` / `Stable` / `Lossless`），直观展示帧长（`5ms`/`10ms`）、编码器（`Opus`/`PCM16`）、`FEC` 状态与目标缓冲大小。
4. **Bento 遥测信息砖 (`metric_tile`) 与状态胶囊 (`status_pill`)**
   - 统一封装三列/双列内凹数据砖块与带呼吸圆点的状态胶囊（`STREAMING`、`LISTENING`、`DUPLEX LINK ACTIVE`）。
