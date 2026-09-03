# ADR-0001: TTS 引擎多后端解耦与进程契约接缝

## 日期
2026-09-04

## 状态
accepted

## 背景

voice-core 是专为 AI Agent 设计的本地语音输出运行时。当前版本（v2）采用 Irodori-TTS v4.1-Small 作为默认且唯一的合成引擎。Irodori 在日语语音合成表现出色，但 voice-core 本身并非针对单一日语引擎设计，未来预期会接入针对其他语言（例如中文、英语）或不同声学特性的其他 TTS 引擎后端。

在早期的开发历史中，已先后有 IndexTTS、Qwen3-TTS、Irodori 三款引擎接入过 voice-core。因此，架构上明确需要一条稳定、跨语言、进程隔离的后端解耦接缝（Seam），使得增加或替换 TTS 引擎不需要重构核心服务、重写客户端或引入复杂的动态链接库（FFI/ABI）。

同时，必须根据当前代码库（`src/engine.rs`, `src/config.rs`, `src/supervise.rs`, `src/service.rs`, `src/packs.rs`）的实际实现，客观审视现状与理想接缝之间的差距，避免虚假抽象。

### 现状代码实测与走读分析

1. **`src/engine.rs` 中的 Trait 与数据结构**：
   - `pub trait TtsEngine` 定义了四个核心异步方法：`health`, `load_model`, `unload_model`, `synthesize`。
   - `TtsEngine` **并非 Object-Safe（对象安全）**：签名使用 `impl Future<Output = ...> + Send`，代码注释明确指出“当前 binary 内只有单一引擎，动态分发（dynamic dispatch）没有收益且每调用增加一次堆分配”。这意味着目前无法直接构建 `Box<dyn TtsEngine>`。
   - `EngineHealth`（`ready`, `model_loaded`）与 `SynthOutput`（`sample_rate`, `duration_ms`）以及 `EngineError` 均完全具备引擎无关性（Engine-Neutral）。
   - `PackTarget`（`kind: &'static str`, `path: String`）携带 `kind`（`lora-adapter`, `speaker-embedding`, `reference-audio`），适合适配器类模型，但仍较通用。
   - **概念泄漏点**：`SynthRequest` 中明确包含了 `num_steps: u32`（生成步数）。这强绑定了扩散模型（Diffusion）/流匹配（Flow Matching）的超参数概念。对于自回归（Autoregressive）或传统前馈/VITS 模型，`num_steps` 是无意义或不存在的概念。
   - `IrodoriEngine` 是目前 `TtsEngine` 的唯一具体实现，且错误解析函数 `engine_failure` 强依赖于 worker 返回的 `"model load failed: "` 和 `"synthesis failed: "` 前缀。

2. **环回进程协议（Loopback Protocol，见 `docs/api.md` §Engine contract）**：
   - 包含四条 HTTP 路由：
     - `GET /health` -> `{"ready": bool, "modelLoaded": bool}`
     - `POST /load` -> `{"modelLoaded": true, "loadMs": int}` / `{"error": "model load failed: ..."}`
     - `POST /unload` -> `{"modelLoaded": false, "freedMs": int}`
     - `POST /synthesize` (输入 `text`, `outPath`, `voicePack`, `seed`, `numSteps`；输出 `sampleRate`, `durationMs`, `bytes`)
   - **引擎中立性评价**：总体架构是高度引擎中立的。特别是“runtime 指定 spool 路径，worker 直接写 WAV，不经过 JSON Base64 搬运”和“三阶段控制（ready / model_loaded / unload 显存回收）”的设计非常干净。唯一的泄漏依然是 `/synthesize` 请求体中的 `numSteps`。

3. **`src/config.rs` 中的配置模型**：
   - `WorkerSource`（`Managed` vs `External`）中立。
   - `WorkerSpec`（`python: PathBuf`, `script: PathBuf`, `root: Option<PathBuf>`, `env: Vec<...>`, `placement: EnginePlacement`）：
     - **强绑定 Python Worker**：字段直接假定 worker 必须通过 python 解释器执行某个脚本启动。如果未来后端是编译型单二进制程序（C++/Go/Rust），`python` 字段将产生语义冲突。
   - `EnginePlacement`（`model_device`, `codec_device`, `model_precision`, `codec_precision`）：
     - **强绑定神经音频 Codec 架构**：分离了 `model` 与 `codec` 的设备和精度，这是典型 Irodori/DACVAE 架构的产物。

4. **`src/supervise.rs` 中的进程监督**：
   - `Worker` 结构体中直接内嵌了单实例具体的 `engine: IrodoriEngine`，并暴露 `pub fn engine(&self) -> &IrodoriEngine`。
   - `Worker` 内部的 `running: Mutex<Option<Running>>` 仅监督单一子进程生命周期。

5. **`src/packs.rs` 与 `src/service.rs` 中的路由状态（关键发现）**：
   - 在 `src/packs.rs` 的 `VoicePack` 结构体中，声明了 `pub engine: String` 和 `pub languages: Vec<String>`，并在 `config.json` 中配置（如 `"engine": "irodori-tts-v4.1-small"`, `"languages": ["ja"]`）。
   - **但在 `src/service.rs` 中检查 `resolve_pack` 与 `speak` 的执行流**：
     - `resolve_pack` 仅按 `id` 查找声线包并转换为 `PackTarget`。
     - **代码完全没有对 `pack.engine` 或 `pack.languages` 进行任何检查、过滤或路由分发！**
     - 结论：**`engine` 和 `languages` 在当前代码中完全是惰性元数据（Inert Metadata）**，尚未激活任何路由逻辑。

## 决策

我们选择：**以四条环回 HTTP 控制路由为跨进程接缝，以声线包中的 `engine` 字段为路由键，支持多后端插拔；但暂不构建推测性多后端代码。**

核心准则如下：
1. **进程边界即插件接缝**：任何 TTS 后端均作为独立子进程运行，遵循 `/health`, `/load`, `/unload`, `/synthesize` 四条标准 HTTP 路由契约，通过 stdout 报告结构化生命周期，向 runtime 指定的 `outPath` 写入标准 WAV。不使用 In-Process 动态库（dll/so/C-ABI）。
2. **声线包 `engine` 是路由键**：每个声线包的 `engine` 字段（如 `"irodori-tts-v4.1-small"`）用于定位其对应的后端 worker 实例。请求未指定 voicePackId 时，路由至全局默认引擎。
3. **语言归属后端，而非 voice-core 核心**：当前 voice-core 默认搭载的 Irodori 后端最擅长日语；voice-core 本身定位为多语言多后端的调度运行时。
4. **不进行过早抽象**：在第二个具体引擎后端引入之前，不编写抽象的多进程路由胶水层或虚构的通用配置解析器。

## 后果

### 正面
- **零 FFI/ABI 负担**：不同引擎可自由采用不同 Python 版本、不同 PyTorch/CUDA 依赖、乃至使用 C++ 或 Rust 编写，彼此依赖完全隔离。
- **崩溃隔离**：特定后端的 Segfault、CUDA OOM、依赖冲突被 Win32 Job Object 限制在单一子进程中，守护进程 `voice-core-runtime` 保持健康。
- **协议极简稳定**：仅需四条 HTTP 路由即可完全纳管一个新后端的冷启动、热加载、显存回收与音频生成。

### 负面 / 代价（核心工程难点）
- **显存调度与争用（The Hard Part）**：
  - 目前 runtime 的显卡互斥仅由单信号量控制（`gpu: Arc<Semaphore::new(1)>`），空闲回收（Idle Reclaim）也是以单 worker 为粒度独立计时的。
  - 若同时常驻两个神经网络 TTS 后端，主流消费级显卡（如 8GB/16GB VRAM）将极易发生 OOM。
  - 若采用“互斥加载/分时换出”，在后端 A 与后端 B 之间切换说话将频繁触发模型卸载（`POST /unload`）与加载（`POST /load`），每次切换引入 10–30 秒冷加载延迟，带来严重的请求抖动。未来必须建立全局 VRAM 预算与跨后端排队策略。
- **多子进程生命周期管理开销**：Runtime 需要监督多个子进程树、分配不同端口并追踪不同的日志文件。

## 引入第二个后端时的实施路径（实施顺序清单）

当未来引入第二款 TTS 引擎时，需按以下严格顺序实施工程改造（当前不做）：

1. **解耦 `TtsEngine` Trait 与请求结构**：
   - 将 `SynthRequest.num_steps` 改为可选或移动到引擎私有参数字典中；
   - 评估 `enum BackendEngine`（枚举静态分发）或改用 `async_trait` 包装进行动态分发。
2. **重构配置体系与 Worker 描述符**：
   - 改造 `src/config.rs` 中的 `WorkerSpec`，将其抽象为后端启动规范（支持不同的可执行文件、命令行模版与参数映射，消除硬编码的 python 脚本假定和 DACVAE 专有配置）。
3. **改造进程监督器（Supervisor）**：
   - 将 `Worker` 升级为 `WorkerPool` 或 `EngineSupervisor` 映射表，支持按 engine ID 懒加载和按需管理多个进程端口。
4. **激活 `src/service.rs` 中的引擎路由**：
   - 在 `resolve_pack` 中提取 `pack.engine`；
   - 将 `pack.engine` 作为键路由至对应后端的 Supervisor；
   - 校验请求语言是否与声线包支持的 `languages` 兼容。
5. **设计跨引擎显存置换策略（VRAM Scheduling Policy）**：
   - 升级 `gpu: Semaphore(1)` 机制，引入“后端切换预检”：当即将执行调用的后端尚未加载模型且显存受限时，先调用上一后端的 `/unload` 释放 VRAM，再执行当前后端的 `/load`。

## 备选方案

- **方案 A：基于 C-ABI / 动态链接库的 In-Process 插件机制**
  - *为何放弃*：不同 TTS 引擎的深度学习环境（PyTorch/TensorRT/ONNX Runtime 版本、CUDA 运行时版本）极易在同一进程地址空间内产生符号冲突或 DLL 地狱，且引擎崩溃直接导致 runtime 宕机。
- **方案 B：通用 gRPC / IPC 通信**
  - *为何放弃*：增加了 Protobuf 编译链和客户端桩代码维护成本。当前基于 Loopback HTTP JSON + 本地文件系统 Spool 的机制已经足够高效（合成耗时主要在 GPU 计算，HTTP 环回开销可忽略）。
