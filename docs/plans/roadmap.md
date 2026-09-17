# Roadmap

进行中的产品是 v1.2.0（已发布：Obsidian 配色 + 新标识 + WinUI 3 几何 + 部署屏 + 本地资源测量 +

音色包规范）。这里只写**还没做**的东西，每条都写清楚"为什么现在不做"和"做的时候第一步是什么"。

- 角色卡（persona/character card）：[persona-roadmap.md](./persona-roadmap.md) —— 把酒馆式角色卡塞进音色包，任何 harness 开会话自动加载人设（2026-09-09 定稿，未开始）。

---

## 已落地的地基（影响下面每一条，先读）

**配置只有两处，包内优先**（`docs/voicepack-spec.md`，1.2.0）：`data/config.json` 管全局与
"装了哪些包、在哪"；`<包>/voicepack.json` 管这一个包。同字段冲突时包内赢——注册表条目是安装器和
面板生成的，不该盖过包对自己的描述。没有第三层"程序默认"：两处都没写的字段是**推导**（`name`←`id`、
`kind`←载荷）或**内置行为**（无头像时取 `character` 首字），不是配置。

`dialog` 与 `synthesis` 两节**已经能被解析、合并、通过 `/api/voices` 暴露**，但字幕端和 worker
还没消费它们——这正是 R1 与 R2 各自剩下的那一半。

**安装树自带引擎**（1.2.0）：解释器、引擎源码、HF 缓存分别在 `runtime/python`、`runtime/engine`、
`models/huggingface`，即运行时本来就会自己找的位置，所以本机 `runtime.json` 只剩 `idleStopSecs`。


---

## 本轮在做（1.3.0，五条并行）

触发这一轮的有两件事：用户要求「音色包做好 + 显存与速度下大力气 + 分发模式不变」，以及一次外部
agent 评审（Gemini）从**调用方**角度指出的四个摩擦点。评审的取舍如下，理由写在这里而不是散落在
提交里：

| 评审意见 | 处理 | 理由 |
|---|---|---|
| 缺「等播完」的同步闭环，只能 `time.sleep(7.5)` 猜 | **做**（R4） | 连续朗读是真实用法，而猜延时是错的；`speak --wait` + 播放生命周期事件 |
| 长朗读需要停顿 | **做**（R4） | 文本里一个 `[pause:N]` 原语，切段合成后拼 N 毫秒静音，一个 audioId |
| Windows 下 `--ruby-pairs` 的 JSON 转义地狱 | **只做一半** | 加 `--ruby-pairs -` 从 stdin 读，绕开引号；不加自动分词 |
| 分词对齐的认知成本压在调用 LLM 上 | **不兜底** | 用户的明确取向：这份重量留给调用方。我们能做的是**失败显式**——对不上时报出第一个分歧的下标和两边的实际字符串，而不是悄悄退化成单行 |
| 单引擎、只会日语，中文会读成乱码 | **打地基**（R5） | 现在加 `language` 字段与 `voice_language_unsupported` 错误码：说得出、验得住、拒得掉；真正的多引擎路由等有第二个引擎再做 |
| 一次一句、冷启动 20 s、并发即 `resource_busy` | **部分**（R2） | 冷启动与单句延迟是 R2 的正题；并发不做——GPU 单租户是有意的，见 C4 |

五条并行的分工（文件所有权互斥，集成点由主代理串行合并）：

1. **应用内训练**：`scripts/training/**` 五个脚本加 `--json` 事件流（协议与 `bootstrap.ps1 -Json`
   逐键相同），`provision.rs` 的行流式子进程 runner 抽成 `jsonstream.rs` 供两边共用，面板加
   「训练」屏：语料 → 参数 → 六段进度 → checkpoint 选择 → 安装。tqdm 在 **Python 里**解析——
   它是产出方，Rust 只转发。
2. **配置可视化**：provenance 由 `packs.rs::hydrate` 在合并时直接产出 `sources` 映射
   （`pack`/`config`/`derived`），随 `/api/voices` 出去；面板只渲染，不重算优先级。
3. **调用契约**（R4）：播放闭环、`[pause:N]`、ruby stdin + 严格错误、`language` 字段。
4. **引擎性能**（R2）：先测后改。已知死路不碰：fp16（引擎拒绝）、卸 ModernBERT（在 DiT
   checkpoint 里）、CPU offload（5–10 倍延迟）、文本前处理缓存（<2.5 ms）。要打的是
   reserved 比 allocated 高的那 1.4 GiB、占单句 1300/1600 ms 的 `sample_rf`、以及
   13.7–23.1 s 的模型加载。
5. **视觉**：JSON 视图、来源标签、生效值表、六段 wizard、长任务控制台的 CSS 词汇。

### 这一轮测出来的东西（1.3.0 已落地，剩下的记在 R2）

先测后改，结果与直觉不同，所以把数字留在这里：

| 项 | 之前 | 之后 | 说明 |
|---|---|---|---|
| 模型加载（热盘） | 23.0 s | **19.2 s** | 建 DiT 时不再初始化随后就被 `strict=True` 覆盖掉的权重 |
| 加载后 reserved 显存 | 3178 MiB | **2078 MiB** | 加载完做一次 `empty_cache()`，放掉 fp32→bf16 的瞬时峰值 |
| 单句 peak reserved | 3468 MiB | **2408 MiB** | 同上 |
| 单句延迟 | 3409 ms | 3558 ms | 没变；+4% 是整轮偏移，同一轮里 `predict_duration` 也漂了 3%，而它不可能被这次改动影响 |
| 音频 | — | **逐字节相同** | 两处改动都不碰计算 |

已经排除的死路（都有实测，别再试）：`expandable_segments` 在 Windows 上直接不支持（PyTorch 自己
warning），另两个 allocator 旋钮测出来三位数字完全一样——那 1.4 GiB 从来不是碎片；CFG 批处理和
SDPA 注意力**上游早就做了**（`rf.py` 每步只发一次 forward，`model.py` 四处都用
`scaled_dot_product_attention`）；fp16 被引擎拒绝；ModernBERT 在 DiT checkpoint 里拆不出来；
CPU offload 是 5–10 倍延迟；文本前处理缓存只值 2.5 ms；bf16 副本 checkpoint 只省 1.3 s 却多 1.53 GB
磁盘，按自己的数字否掉。

### R2 剩下的（按证据排序）

1. **采样器是 CPU 分发瓶颈，不是 GPU 瓶颈。** 每个 Euler 步约 7200 次 ATen 分发；把候选数从 1 提到
   4（即 4 倍 DiT 计算量）延迟只涨 1.3%，说明显卡在等 Python。回归式：
   `sample_rf ≈ 179 ms + 96 ms × steps`。**CUDA graphs 能吃掉这部分，头寸约 30 倍**，但正确的落点是
   上游 `rf.py` 的 Euler 循环——我们不改安装树里的引擎。这条要么给上游提 patch，要么在 fork 里做。
2. **`torch.compile` 走不通**：`TritonMissing`，而给发行版加 triton 属于改分发模式，已冻结。
3. **上游有个双算**：`encode_conditions` 每次合成跑两遍（`inference_runtime.py:1281` 与
   `rf.py:220`，参数相同），约 140 ms。修它要改上游函数签名。
4. **`numSteps` 32 → 20 的取舍留给人耳**：20 步延迟只有 0.68×，与 32 步的余弦相似度 0.9969、
   相似度下界 0.4967 vs 0.5022（在种子噪声内），但 mel-dB RMSE 1.85 是真实的频谱差异，**没人听过**。
   36 段对比音频在 `data/bench/steps/`。默认仍是 32；要改就先听，或者以后让音色包用
   `synthesis.numSteps` 自己说（R1 消费那一节时顺手就能通）。
5. **训练那条链有个环境陷阱值得记住**：`datasets` 4.x 只用 `torchcodec` 解音频，而 `torchcodec` 没有
   FFmpeg 就加载不了原生库。1.3.0 已经把 latents 步改成自己在进程内调引擎的 codec（产出与上游逐字节
   相同），所以训练不再依赖这条链；但任何以后想用 `datasets` 读音频的功能都会再撞上它。

---

## R1 · 让字幕端真的用上包里的 `dialog`（数据面已通，消费面未做）

### 目标

让每个音色包自带一套字幕外观，而不是所有角色共用一份全局配置：

- 说话人名字的颜色
- 倒计时条的颜色
- 正文颜色，且**两种语言可以不同颜色**（`text` 与 `displayText` 分别着色）
- 文字显示方式（逐字/整句、停留秒数）
- 优先级：**音色包内配置 > `config.json` 的 `dialog` 节**（已是全局规则，见上）
- CLI 可用参数临时覆盖（`--name-color`、`--text-color` 之类）

### 已经做完的部分（1.2.0）

1. `src/packs.rs` — `dialog` 段已在规范里，已解析、已按"包 > 条目"合并、已有兼容策略
   （未知字段忽略、schema 更高只警告一次）和三条单元测试。
2. `/api/voices` — 已把合并后的 `dialog` 连同 `manifest` 来源一起暴露。

### 剩下的部分

3. `src/obs.rs` — `speech` 事件要把解析后的 `dialog` 一起推给字幕端；这个事件是 agent 可读的公开
   契约（`docs/api.md`、`SKILL.md` 都写了字段），加字段等于改 API。
4. `app/VoiceCoreTray/Dialog/DialogTheme.cs` — 现在是 `static readonly` 常量，要改成"每次 speech
   事件可覆盖的实例"，`ApplyTheme()` 要能重复调用而不闪。
5. `src/bin/voice-core.rs` + `src/service.rs` — CLI 参数 → `SpeakInput` 新字段 → 事件。
6. 面板：音色页展示/编辑包内 `dialog`（后端钩子 `pack_manifest(id)` 已经在，返回原始 JSON）。

估：**半天**（比 1.2.0 之前的估算少一半，因为 1 和 2 已经不用做了）。中途做一半会让
"agent 读 SKILL.md 就能调用"这条线不一致——要么整条做完，要么不开始。

### 做的时候第一步

先在 `docs/adr/` 写一条 ADR：**`dialog` 怎么到达字幕端**。候选方案：
- (a) 事件带完整解析结果 → 字幕端零逻辑，但事件体积变大
- (b) 事件只带 `voicePackId`，字幕端自己读盘 → 事件不变，但字幕端要有配置读取与热重载，
      而且要重新实现一遍"包 > 条目"的合并——现在这个合并只有 runtime 一份
- (c) 混合：事件带**覆盖项**，其余走字幕端默认

(b) 最省契约改动，但要复制合并逻辑，这与"只有一处实现优先级"相悖；(a) 对多前端最友好。
这个决定不做完，代码写哪都是错的。

---

## R2 · 显存与内存占用优化（未开始，先测量）

### 现状（1.2.0 实测，RTX 5060 Ti 16 GiB / Windows 11）

| 项 | 数字 | 来源 |
|---|---|---|
| 引擎进程工作集 | **约 4.3 GiB** | `GetProcessMemoryInfo`，状态页「内存」卡 |
| runtime 服务 | 约 10 MiB | 同上 |
| 整卡显存占用（模型驻留时） | 约 7.4 / 16 GiB（含桌面与浏览器） | `nvidia-smi --query-gpu=memory.used` |
| 引擎自己的显存 | **测不到** | GeForce 在 WDDM 模式下 `--query-compute-apps=used_gpu_memory` 一律返回 `[N/A]` |

### 第一步不是优化，是把显存测准

没有per-process显存数字，任何"优化了多少"都无法证明。两条路：

- **PDH GPU 计数器**：任务管理器就是这么显示每进程显存的，计数器路径 `\GPU Process Memory(pid_<PID>_*)\Dedicated Usage`。Rust 里用 PDH API（`PdhOpenQuery`/`PdhAddCounter`），或先用 `Get-Counter` 验证可行性。这是**唯一**在消费级 Windows 上拿到真数字的办法。
- **让引擎自己报**：worker 里 `torch.cuda.memory_allocated()` / `memory_reserved()`，通过 runtime 的 `/api/status` 透出。数字最准（区分 allocated 与 reserved），但要动 worker 协议——和 R1 一样是契约变更。

建议先做 PDH：不动任何契约，只在 manager 里加一个测量源。

### 可能的优化项（按预期收益排序，都需要先有测量）

1. **dtype**：确认模型是否已经是 fp16/bf16。若还有 fp32 权重，减半是最大的一笔。
2. **ModernBERT 常驻**：`sbintuitions/modernbert-ja-310m` 只在文本前处理用一次。合成后卸掉它、下次按需加载，代价是每句多几百毫秒——**这是典型的"体验换显存"，要实测再决定**。
3. **`torch.cuda.empty_cache()` 时机**：现在空闲 15 分钟卸模型。可以在每次合成结束后释放 reserved 但不 allocated 的那部分，缓存命中会掉一点。
4. **DACVAE 与主模型的生命周期分离**：声码器比主模型小得多，可以常驻；主模型按需。
5. **CPU offload**：`accelerate` 的 device_map 把不活跃层放内存。首包延迟会涨，和 15 分钟卸载策略打架。

### 验收口径（写死，不然会自欺）

每一项改动都必须报：`基线显存 / 基线首包 / 基线单句 → 改后三个数字`，测 5 次取中位数，同一段文本、同一音色包、同一 seed。
体验退化超过"首包 +15% 或单句 +10%"就不接受，除非显存收益大于 1 GiB。

---

## R3 · 打包与分发的收口（1.2.0 已做一半）

`scripts/package.ps1` 是**构建器**，不是可选的打包方式：它跑 cargo / dotnet / tauri，组装 `dist/voice-core` 那棵树，
而 Inno 的 `.iss` 用 `{#SourceTree}\*` 递归收那棵树。删掉它，安装器就没有东西可压。

真正冗余的是它的**便携分发选项**，既然 setup.exe 是唯一分发手段：

- `-Zip`：产出 `dist/voice-core.zip`。没有分发渠道用它。**已删（1.2.0）**
- `-SkipGui`：产出没有入口点的树，是 GUI 还没写完时的脚手架。**保留**——它同时是「GUI 编译挂了但其余部分要能打包」
  的逃生口，而且与布局断言（`$want`）在六处耦合，拆掉的代价大于收益。它自己会大声警告，不会被误用。
- `-IncludeEngine` / `-IncludeModels`：仍然有用（做一台新机器的整盘迁移时），但 setup.exe 从不带它们。
  **保留**。1.2.0 起它们不再有默认输入路径：必须给 `-EngineVenv` / `-EngineRoot` / `-ModelCache`
  或设 `VC_ENGINE_VENV` / `VC_ENGINE_ROOT` / `VC_MODEL_CACHE`，否则直接报错并把该传的路径写在错误里。
  原来的默认值指向一台机器上的 v1 检出目录，那种默认只会在别人机器上骗人。
  本机的三个值就是安装树里的 `runtime\python`、`runtime\engine`、`models\huggingface`。

顺带：安装器目前**未签名**，SmartScreen 会拦。签名要么买证书，要么在 Release 里公布 SHA256（现在 `package.ps1` 已经打印）。

---

## R6 · MeanFlow 蒸馏：自产 4 步推理模型（上游 89f9d8f 已铺路，未开始）

**一句话**：上游 2026-09-12 的 `89f9d8f`（Add MeanFlow distillation and v4-Large support）
给了把 RF 模型蒸成 4 步 MeanFlow 模型的完整配方。自己跑一次蒸馏，得到 Small-MF 模型后，
采样从 32 步降到 4 步——`sample_rf`（实测 ≈461 ms，回归式 `179 ms + 96 ms × steps`）
理论缩到几十 ms，单句 636 ms 里的大头直接砍掉。这是**推理侧**收益，与 LoRA 音色包训练无关。

### 为什么当时没合并、现在排进来

上游 `89f9d8f` 与 fork 的 `voice-core` 分支在 `irodori_tts/inference_runtime.py` 采样分派点
有 1 处冲突（恰好砸在 Patch 1 的 `encoded_conditions` 传参上）；`model.py` 虽然自动合并干净，
但上游重写了 attention dispatch（新 `attention.py`：FA3 需要 Hopper，5060 Ti 是 Blackwell
只能走 cuDNN-first SDPA 回落），CUDA Graph patch 包住的 forward 内容变了——合并后必须重跑
逐位一致性与 bench（FORK.md 的验收标准：15 clips bitwise identical）。当时没有 checkpoint 侧
的收益，合并不挣得验证成本。**现在决定自己蒸馏，合并从"可选"变成"前置"**——没有上游的
`sample_euler_meanflow` 和 `flow_parameterization` 分派，蒸馏出的模型没有推理代码可跑。

### 蒸馏配方已具备的条件（都是现成的）

- teacher = 本地 v4.1-Small RF checkpoint（`model.safetensors` 元数据自带 `config_json`，
  蒸馏配置直接引用，无需重新导出）
- 训练语料 = 训 LoRA 音色包用的那套语料管线（`scripts/training/` 的 prepare/encode 流程
  产出的 latents 与 manifest 直接复用；meanflow 配置吃的是同一格式 manifest）
- 配置文件上游已给全：`configs/train_v4_small_meanflow.yaml`（batch 40 × grad-accum 2、
  bf16、muon、50000 步、`teacher_steps: 40`、teacher KV cache 分块）
- 模型很小（model_dim 1280 / 12 层），student + 冻结 teacher + KV cache 单卡可容
  （16 GiB 卡，teacher KV 分块由 `meanflow_teacher_chunk_size` 控制）；上游支持
  teacher 与 student 同卡（`meanflow_teacher_device_offset: 0`）

### 顺序（合并先行，蒸馏在后）

1. **合并上游 `89f9d8f` 进 `voice-core` 分支**。冲突一处：inference_runtime.py 采样分派——
   把 Patch 1 的 `encoded_conditions` 传参移植进上游的 meanflow/rf 双分支结构（rf 分支保留
   去重；meanflow 分支不需要——CFG 在蒸馏时已融合，但 duration 分支的六元组缓存仍然要接上，
   否则 MeanFlow 路径上 encode_conditions 还是跑两遍）。model.py 自动合并后 CUDA Graph
   patch 包住的 forward 变了（上游重写了 attention dispatch），**必须重验**：15 clips
   bitwise identical + bench 数字不回退。
2. **跑蒸馏**：按上游配方，manifest 用现有训练语料；验收 = 学生模型 4 步合成的听感对比
   （36 段盲听对齐 R2 里 numSteps 的做法，mel-dB RMSE + 人耳），不能只看 loss。
3. **实装**：Small-MF 进模型清单（worker 的模型解析已按 checkpoint 元数据自动选采样器，
   理论零改动），面板「模型」行报新名字；LoRA 音色包在 MF 基座上重训或验证 LoRA 迁移性
   （这一点有风险：MeanFlow 的 DiT 权重变了，旧 LoRA 大概率失效——音色包要重训，排期时算进去）。
4. **分发**：0.1.x 的模型清单加 Small-MF（可选下载，不是替换——RF 版仍是兼容底座），
   bootstrap 的 models 表加一行，setup 引导装机自动可选。

### 验收口径（沿用 FORK.md 标准）

- 合并后：15 clips bitwise identical（两个 patch 在上游新树上仍然逐位一致）
- 蒸馏后：4 步合成 vs 32 步 RF，mel-dB RMSE + 人耳盲听；`sample_rf_ms` 中位数从 ≈461 ms
  显著下降，**单句总延迟目标是 <300 ms**（从 636 ms）
- 任何一步不达标就停在原地：RF 636 ms 是已验证的底线，MeanFlow 是增量不是替代
