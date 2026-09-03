---
name: voice-core
description: 用本地 voice-core runtime 让 agent 真的发声：指定角色声线（LoRA/SE 声线包）合成日文语音，自动播放并弹出中文字幕对话框。当用户要求“和我说话”“用某角色的声音念/回复”“语音回复我”时使用；也包含配置项说明与新声线包的训练与注册流程。
---

# voice-core：用角色声线对用户说话

你（agent）通过本机 `voice-core` runtime 获得真实语音输出：合成 → 自动播放 → 自动弹字幕。
模型、Python、端口、显存调度全部封在 runtime 里，你只走本文档的接口。

## 0. 核心约定（必读）

- **两段文本各有其职**：`text` 是**念出来的日文**，`displayText` 是**给用户看的中文**。
  两者都由你生成——翻译是你的责任，runtime 从不翻译。只有 `text` 是必填。
- **对齐由你提供**：`rubyPairs` 是 `displayText` 与 `text` 的分段对应关系
  （`base` 是中文片段，`ruby` 是它对应的日文片段，按阅读顺序，两侧各自拼接必须还原完整原串）。
  中日文不按位置对齐（SOV vs SVO，翻译会合并/拆分/重排子句），下游无法推导，
  所以只有你能给。不给也能工作，对话框会退化成整行旁注。
  **标点照样成对给**（`{"base":"，","ruby":"、"}`）：拼接规则要求它们在，
  渲染端会自己丢掉纯标点的旁注,不需要你预处理。
- **播放和字幕全自动**：调用 speak 之后托盘（VoiceCoreTray）自动朗读并弹出对话框。
  你不写任何播放代码。用户说“听不到”→ 先确认托盘在运行、菜单里“自动播放声音”是勾选的。
- **音频不走 JSON**：`/api/speak` 只回 `audioId`，字节从 `GET /api/audio/{audioId}` 取。
  全系统没有 base64。一般你不需要取字节。
- **令牌**：`Authorization: Bearer <token>`，token 在数据目录的 `token.txt`。
  发行态数据目录是安装根下的 `data\`；装在不可写目录（Program Files）时 runtime 会退到
  `%APPDATA%\voice-core`。CLI 会自己按 `--token` → `VC_TOKEN` → 数据目录逐级找，
  用 CLI 就不用管这件事。

## 1. 确保服务在运行

```powershell
bin\voice-core.exe doctor    # 可达性、鉴权、引擎状态、声线包，一次看全
bin\voice-core.exe status    # JSON：runtime / worker / spool 明细
```

用户装态直接双击 `bin\app\VoiceCoreTray.exe`（托盘会自己拉起 runtime）。

诊断要点：`GET /api/health` 是唯一免鉴权路由。**health 通而 status 返回 401 = 令牌不匹配**
（托盘与 runtime 的数据目录不一致），不是“服务没起”——此时再启动一个实例只会撞端口 8760。

## 2. 说话

**首选：一行 CLI。**自动找令牌、合成、交给托盘播放和弹字幕：

```powershell
bin\voice-core.exe speak `
  --voice ba-shun-kid-lora `
  --text "おかえりなさい、先生。" `
  --display "欢迎回来，老师。" `
  --ruby-pairs '[{"base":"欢迎回来","ruby":"おかえりなさい"},{"base":"，","ruby":"、"},{"base":"老师。","ruby":"先生。"}]'
```

`--ruby-pairs @pairs.json` 可从文件读数组，长句更省事。
输出一行 `displayText` 供你核对，外加一行 `requestId | 耗时 | presenter 数`。
`--play auto|always|never`（默认 `auto`：只有在没有其他前端订阅事件流时才自己播）。
`--out <path>` 保留 WAV。首次合成含模型加载（可达数十秒），热调用约 1.5–4 s。

**HTTP（需要自己控制时机或拿字节时用）：**

```bash
curl -s -X POST http://127.0.0.1:8760/api/speak \
  -H "Authorization: Bearer $(cat data/token.txt)" \
  -H 'Content-Type: application/json' \
  -d '{
    "text": "おかえりなさい、先生。",
    "displayText": "欢迎回来，老师。",
    "rubyPairs": [{"base":"欢迎回来","ruby":"おかえりなさい"},
                  {"base":"，","ruby":"、"},
                  {"base":"老师。","ruby":"先生。"}],
    "voicePackId": "ba-shun-kid-lora"
  }'
```

可选字段：`seed`、`numSteps`、`displaySeconds`（字幕停留秒数）、`timeoutMs`。
其他路由：`/api/voices` 列已装声线包、`/api/warm` 预加载模型、`/api/sleep` 释放显存、
`DELETE /api/requests/{requestId}` 取消（**注意**：立刻放开调用方，但引擎会跑完当前 step
才真正释放 GPU，不做逐步中断）。

## 3. 事件流（只在你要自己做前端时才需要）

`GET /api/events` 是 SSE，每帧一个 JSON 信封；新订阅者先收到最近 64 条尾巴。
`kind` 取值：`runtimeReady` / `runtimeStopping` / `workerStarting` / `workerReady` /
`workerStopped` / `speakStarted` / `speech` / `speakFailed` / `progress`。
`speech` 一帧包含字幕与播放所需的全部字段（`audioId`、`text`、`displayText`、
`rubyPairs`、`durationMs`、`sampleRate`）。runtime 从不反向调用前端，只有这一条推送通道。

## 4. 错误处理

每个非 2xx 都是同一形状：`{code, message, recovery:{kind, detail}}`。**按 `code` 分支，不要看文案。**

| `code` | HTTP | 你的动作 |
|---|---|---|
| `unauthorized` | 401 | 重读 `token.txt`；health 通则是数据目录不一致 |
| `voice_pack_not_found` | 404 | `recovery.detail` 里有已装 id 列表，如实告知,不要编造 id |
| `not_found` | 404 | `audioId` 过期（spool 按时间和总量淘汰，重启即清），重新合成 |
| `worker_unavailable` | 503 | 外挂引擎没应答 |
| `worker_start_failed` | 500 | 引擎起不来；消息里带实际等待毫秒数与 stderr 尾巴 |
| `model_load_failed` | 500 | 引擎起来了但模型加载失败；消息里有引擎自己的原因 |
| `resource_busy` | 429 | 设备队列拒绝，退避重试 |
| `deadline_exceeded` | 504 | 超过 `timeoutMs`，给更长超时重试一次 |
| `cancelled` | 499 | 调用方自己取消的 |
| `internal` | 500 | 其他；消息里带引擎原文，`recovery` 指向引擎日志 |

`recovery.kind` ∈ `retry` / `wait` / `check_token` / `check_worker_logs` /
`install_voice_pack` / `fix_request`。

日志：`data\logs\runtime.{out,err}.log`、`tts-worker.{out,err}.log`、
`dialog.jsonl`（对话框每句一行指标）、`data\metrics.jsonl`（合成延迟）。

## 5. 配置：全在一个文件

`data\config.json`，托盘菜单 **设置（含声线包）** 直接打开它。
允许 `//` 注释和尾随逗号，也容忍 UTF-8 BOM。三段：

```jsonc
{
  "dialog": {
    "annotationAbove": false,   // 旁注在正文上方还是下方
    "reveal": "typewriter"      // typewriter 逐字（按音频配速）| sweep 柔光扫过 | fade 按子句淡入
  },
  "hotkeys": {
    "toggleDialog": "Ctrl+Alt+D",   // 至少带一个修饰键
    "toggleHold":   "Ctrl+Alt+H"    // 常驻 / 倒计时自动隐藏
  },
  "voicePacks": [ /* 见下一节 */ ]
}
```

`dialog` 和 `hotkeys` 改完要重启托盘；`voicePacks` 由 runtime 按 mtime 自动重载，**不用重启**。

## 6. 新声线包：制作与注册

### 6.1 两种 kind，两种产物

| `kind` | 产物 | `path` 指向 | 说明 |
|---|---|---|---|
| `lora-adapter` | **目录**，含 `adapter_config.json` + `adapter_model.safetensors` | 该目录 | 学韵律和风格，质量最好 |
| `speaker-embedding` | **单个文件，文件名必须以 `.speaker.safetensors` 结尾** | 该文件本身 | 只换说话人条件，不改基础模型 |
| `reference-audio` | 参考音频 | 音频文件 | 零训练，音色够但学不到风格 |

**已知坑（真实踩过）**：把 SE 文件改名成没有后缀的 `ba-shun-kid-se` 会被引擎拒绝，
报 `Speaker Inversion embeddings must use the '.speaker.safetensors' suffix: <path>`。
拷进 `data\voicepacks\` 时**保留原文件名**。

### 6.2 注册（写进 `config.json` 的 `voicePacks`）

```jsonc
{
  "id": "ba-miyu-lora",              // speak --voice 用的就是它
  "name": "霞沢美游 (LoRA)",
  "character": "霞沢美游",            // 对话框显示的说话人名
  "avatar": "avatars/ba-miyu.png",   // 相对数据目录，或绝对路径
  "languages": ["ja"],
  "kind": "lora-adapter",
  "path": "voicepacks/ba-miyu-lora", // 相对数据目录（便携）或绝对路径
  "engine": "irodori-tts-v4.1-small"
}
```

存盘后 runtime 自动重载，`voice-core.exe voices` 应立刻列出它。

### 6.3 训练一个 LoRA 声线包

训练树在开发仓 `tts\irodori-tts\`（自带 uv venv、HF 缓存、上游 WebUI）：
`env\Scripts\python.exe` 是解释器，`webui\Irodori-TTS\train.py` 与 `infer.py` 是上游入口
（`train.py --config --manifest --init-checkpoint`；`infer.py --checkpoint --ref-wavs --ref-embed --lora-adapter`），
**不要自己写训练循环**。

1. **准备语料。** 日文音频配**日文**转写。中文转写配日语音频会造成文本域错位，
   将来用日文文本生成时 LoRA 学到的是错域映射——这条踩过。
   实测可用量级：约 60–70 条/角色、总时长 ~15 分钟即可出可用声线。
2. **建 manifest**（`data\*_manifest.jsonl`，一行一条）：
   ```json
   {"text": "…", "audio_path": "…\\xxx.wav", "speaker_id": "ba_shun_kid", "latent_path": "…\\0000.pt", "num_frames": 39}
   ```
   latent 由 `data\encode_*_latents.py` 预编码。
3. **离线环境变量必设**：`HF_HOME` / `HF_HUB_CACHE` 指向本地缓存 + `HF_HUB_OFFLINE=1`，
   否则 DACVAE/codec 会去找远端 repo id 报 `FileNotFoundError`。
   uv venv 默认没有 pip，装包用 `uv pip install --python env\Scripts\python.exe <pkg>`。
4. **训练**：抄 `train\shun_lora.yaml`（LoRA `r=16`/`alpha=32`/`target=diffusion_attn`，
   `batch_size=16`、`max_steps=2000`、`precision=bf16`、`lr=1e-4`、`wsd` 调度）。
   **必须显式 `--init-checkpoint` 指向基座 `model.safetensors`**，否则启动即报错。
   实测：RTX 5060 Ti 16GB 上稳态 1.9–2.3 s/step（≈6.4 samples/s），2000 步约 50–90 分钟。
5. **选 checkpoint**：看 val loss，别默认拿最后一步。本例最优是 **1000 步**
   （相似度 0.815–0.818），2000 步已过拟合退化，与 val loss 曲线吻合。
6. **验收**：用与生成模型无关的说话人验证模型（Resemblyzer GE2E）算 d-vector 余弦相似度。
   参考语料留一法均值是“同说话人自然方差”上限基准（本例 0.771，p10 0.703）。
   **看分布下限不看均值**：批量样本最低 0.651 已跌破自然 p10，那就是“多次对话走音”的量化形态。
   脚本在 `eval\similarity_eval.py`。
7. **装包**：把选中的 checkpoint 目录拷进 `data\voicepacks\<id>\`，按 6.2 注册。

**训练性能改动纪律（用户规定，违反即失败）**：先诊断后动手，改前给出吞吐基线+瓶颈证据；
一次只改一个变量并同法复测，无收益就回退；Windows 每个 DataLoader worker ≈700MB 且
persistent 常驻全程，几百条的数据集 `num_workers: 0–2` 足够，
`dataloader_persistent_workers: true` 才是解决 GPU 断粮的关键（占空比 20% → 99–100%）；
改动前声明 RAM/VRAM 增量，不把机器推到 OOM 边缘；不碰正在跑的训练；
汇报必须是“基线数字 → 改动 → 改后数字 → 保留/回退”，禁止“应该会更快”。

### 6.4 只要音色、不训练

WebUI（`tts\irodori-tts\start_webui.ps1`，http://127.0.0.1:7860 ）可直接上传参考音频克隆，
或用已有的 SE 文件。实测 SE / 单参考 / 8 参考的**音色**相似度打平（0.77–0.82）——
音色身份不是瓶颈，**韵律和风格必须靠 LoRA**，这是参考克隆补不上的。

## 7. 边界

- 不要直连 worker 端口（`/api/status` 里的 `worker.port` 是临时端口),一律走 runtime 8760。
- 不要把 token 写进任何会外发的内容。
- 用户要一个不存在的声线时,如实说没有并指向 6.3,不要编造 id。
- 模型权重和声线包不在你的管理范围;需要新声线时引导用户走训练流程。
- runtime 不做 LLM 推理、不做对话管理、不做翻译。
