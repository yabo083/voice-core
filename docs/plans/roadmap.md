# Roadmap

> 唯一事实源。2026-09-17 彻查合并：1.2.0 至 1.9.15 期间所有计划项要么已落地（见归档），
> 要么收敛为下面三条线。版本号已重置为 0.x（保守递增政策），历史发布在仓库 CHANGELOG。

---

## 已落地（归档，按主题）

以下全部**已发布、实机验证**，细节考古看 CHANGELOG 与各 ADR：

| 线 | 交付 | 版本 |
|---|---|---|
| 产品底盘 | Obsidian 配色、WinUI 3 几何、双语面板、托盘/播放生命周期 | 1.2.0-1.7.0 |
| 音色包体系 | voicepack.json 规范、包内>全局合并、provenance（sources 映射）、头像、**面板内 dialog 编辑器**（色彩/逐字-整句/停留秒数，含 unset 继承）、应用内训练屏（六段进度 + checkpoint 安装） | 1.2.0-1.5.0 |
| 调用契约 | `speak --wait` + `waitTimeout` 播放闭环、`[pause:N]`、`--ruby-pairs -`、`language` 字段 + `voice_language_unsupported` | 1.3.0-1.5.0 |
| 引擎性能 | 模型加载 23→19.2s、加载后 reserved 3178→2078 MiB、**fork 两个 patch：条件编码去重 + CUDA Graph 采样**（单句 3558→636ms，1511→636 为 fork 相对上游 main 的收益） | 1.4.0 + fork 36add7d |
| 显存测量 | PDH 每进程 Dedicated Usage（`engineGpuMib` 已上状态页）——原 roadmap 的"唯一拿真数字的办法"已实现 | 1.6.x |
| 自更新 | check/download/install 三命令、prefix 镜像降级、SHA256 + minisign 签名验签全链、断点续传、未完成自恢复、一次直达面板 | 1.8.0-1.9.14 |
| 部署屏 | 三阶段单向链（准备→需下载→完成）、条件页动态显隐、重检测 loading、引擎 fork 从 setup 装机 | 1.9.13-0.1.1 |
| 分发 | package.ps1 构建器 + Inno 单文件、签名双资产、0.1.x 版本重置 | 本周 |

**已排除的死路（都有实测，别再试）**：`expandable_segments`（Windows 不支持）、allocator 旋钮（1.4 GiB 从来不是碎片）、CFG 批处理与 SDPA（上游早做了）、fp16（引擎拒绝）、ModernBERT 拆卸（在 DiT checkpoint 里拆不出来）、CPU offload（5-10 倍延迟）、文本前处理缓存（2.5 ms）、bf16 副本 checkpoint（省 1.3s 换 1.53 GB，否）、`torch.compile`（TritonMissing，加 triton 等于改分发模式）。

---

## 未完成（三条线）

### L1 · MeanFlow 蒸馏：自产 4 步推理模型

上游 2026-09-12 的 `89f9d8f` 给了把 RF 模型蒸成 4 步 MeanFlow 模型的完整配方。自己跑一次
蒸馏，得到 Small-MF 后采样从 32 步降到 4 步——`sample_rf`（≈461 ms，回归式
`179 ms + 96 ms × steps`）理论缩到几十 ms，单句 636 ms 的大头直接砍掉。这是**推理侧**收益，
与 LoRA 音色包训练无关；是"接新后端"的正题。

**为什么合并是前置**：上游 `89f9d8f` 与 fork `voice-core` 分支在 inference_runtime.py 采样
分派点有 1 处冲突（恰好砸在 Patch 1 的 `encoded_conditions` 传参上）；model.py 自动合并干净，
但上游重写了 attention dispatch（新 `attention.py`：FA3 要 Hopper，5060 Ti 是 Blackwell 只能
走 cuDNN-first SDPA 回落），CUDA Graph patch 包住的 forward 内容变了。当时没有 checkpoint 侧
收益，合并不挣得验证成本；**现在决定自己蒸馏，合并从可选变成前置**——没有上游的
`sample_euler_meanflow` 与 `flow_parameterization` 分派，蒸馏出的模型没有推理代码可跑。

**已具备的条件（全是现成的）**：teacher = 本地 v4.1-Small（safetensors 元数据自带
config_json）；训练语料 = LoRA 音色包那套管线（prepare/encode 产出的 latents 与 manifest
直接复用）；配置上游给全（batch 40 × grad-accum 2、bf16、muon、50000 步、teacher KV
分块）；模型很小（model_dim 1280 / 12 层），student + 冻结 teacher + KV cache 单卡可容，
teacher 可与学生同卡（`meanflow_teacher_device_offset: 0`）。

**顺序（合并先行，蒸馏在后）**：

1. **合并上游 `89f9d8f` 进 `voice-core` 分支**。冲突一处：inference_runtime.py 采样分派——
   把 Patch 1 的 `encoded_conditions` 传参移植进上游的 meanflow/rf 双分支结构（rf 分支保留
   去重；meanflow 分支 CFG 已在蒸馏时融合，但 duration 分支的六元组缓存仍要接上，否则
   MeanFlow 路径上 encode_conditions 还是跑两遍）。model.py 自动合并后 CUDA Graph patch
   包住的 forward 变了，**必须重验**：15 clips bitwise identical + bench 数字不回退。
2. **跑蒸馏**：按上游配方，manifest 用现有训练语料；验收 = 学生模型 4 步合成的听感对比
   （mel-dB RMSE + 人耳盲听，对齐 numSteps 的 36 段做法），不能只看 loss。
3. **实装**：Small-MF 进模型清单（worker 按 checkpoint 元数据自动选采样器，理论零改动）；
   **LoRA 音色包大概率要在 MF 基座上重训**（MeanFlow 的 DiT 权重变了，旧 LoRA 失效——
   排期时算进去）。
4. **分发**：0.1.x 的模型清单加 Small-MF（可选下载，不是替换——RF 版仍是兼容底座），
   bootstrap 的 models 表加一行，setup 引导装机自动可选。

**验收口径**（沿用 FORK.md 标准）：

- 合并后：15 clips bitwise identical（两个 patch 在上游新树上仍然逐位一致）
- 蒸馏后：4 步 vs 32 步，mel-dB RMSE + 人耳盲听；单句总延迟目标 **<300 ms**（从 636 ms）
- 任何一步不达标就停在原地：RF 636 ms 是已验证的底线，MeanFlow 是增量不是替代

### L2 · Persona 角色卡

把酒馆式角色卡塞进音色包，任何 harness 开会话自动加载人设。完整设计见
[persona-roadmap.md](./persona-roadmap.md)（2026-09-09 定稿，未开始）：card.md schema、
四个 harness emitter（omp/Claude Code/Codex/Gemini 的 project 级 sentinel 注入）、
`persona use/unuse/show` CLI、`voice-core-persona` skill、mado-kano 实测卡端到端验证。
估一个会话全部落地。**实施时在 voicepack-spec.md 补 persona 一节**（`"persona": "card.md"`
引用行 + card.md 约定），让规范跟上实现。

### L3 · numSteps 32→20 的取舍（碎项，顺手）

20 步延迟 0.68×，余弦相似度 0.9969（种子噪声内），但 mel-dB RMSE 1.85 是真实频谱差异，
36 段对比音频在 `data/bench/steps/`，**没人听过**。听感可接受就改默认（或让音色包用
`synthesis.numSteps` 自己说）。可与 L1 的蒸馏验收一起盲听，一次会话两件事。
