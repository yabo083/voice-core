---
name: voice-core
description: 用本机 voice-core runtime 让 agent 真的出声：合成一句语音、自动播放、弹出字幕对话框，可指定角色音色包。当任务涉及本机语音合成 / TTS / 让 agent 说话或念一段话 / 用某个角色的声音回复 / 音色（音色包、LoRA、音色克隆）/ 字幕弹窗 / voice-core 这个产品本身时使用。内容是最短调用路径、HTTP 契约、音色包怎么选、失败怎么自诊断、新音色怎么注册。
---

# voice-core：让 agent 在这台机器上真的出声

一次调用 = 合成一句 → 播放 → 弹字幕。模型、Python 解释器、引擎端口、显存回收都封在 runtime 里。

**用哪个接口，不是偏好问题：**

- **要说一句话 → 用 CLI** `bin\voice-core.exe`。它自己找数据目录、自己读 token、自己决定要不要放音，
  一行命令就是完整的一次调用；出错时 stderr 直接给 `[错误码] 原因 | try: 建议`。日常让 agent 出声只用这个。
- **要写一个跑在 voice-core-runtime 上的程序 → 才用 HTTP** `http://127.0.0.1:8760`。
  需要流式订阅 `/api/events`、自己管理并发和取消、自己拿 `audioId` 取音频字节、或者把 runtime 当服务集成进
  别的进程时，HTTP 才是对的层。

反过来做都会付代价：用 HTTP 说一句话，你要自己读 token 文件、自己判断 `presenters` 决定要不要放音、
自己处理 401 和冷启动超时——这些 CLI 已经做完了。用 CLI 写集成，你拿不到事件流，只能靠轮询。

## 0. 最短路径

本机安装在 `E:\NewToolBox\voice-core\`：CLI 在 `bin\voice-core.exe`，数据目录在 `data\`。

```powershell
E:\NewToolBox\voice-core\bin\voice-core.exe speak `
  --voice ba-miyu-lora `
  --text "おかえりなさい、先生。" `
  --display "欢迎回来，老师。"
```

实测输出（stdout 一行 displayText，stderr 一行计时）：

```
欢迎回来，老师。
request 75ce8ce2ee774f96 | 22490 ms total (21452 ms synth, cold start) | 0 presenter(s)
```

**冷启动 20–60 秒**：第一句要拉起 Python 引擎进程并把模型搬上 GPU（上面这次 22.5 s）。
模型常驻后一句 1.5–4 s。空闲 15 分钟后 runtime 自己卸模型、空闲 60 分钟后停引擎进程，
所以隔一阵子的第一句又是冷启动。**客户端超时至少给 120 s**，别按"HTTP 一秒该返回"设计；
CLI 自己等 `--timeout-ms / 1000 + 30` 秒（默认 630 s），服务端合成上限默认 600 s。
要把这几十秒挪到用户不在等的时候：先 `voice-core.exe warm`（或 `POST /api/warm`），
它加载完模型才返回，之后的第一句就是热的。

退出码只有三个：`0` 成功，`1` 调用失败（stderr 形如 `[voice_pack_not_found] ... try: ...`），
`2` 参数写错。唯一例外是 `doctor`：它永远退 0，判断要读输出。

不在这台机器上：CLI 一律是安装根下的 `bin\voice-core.exe`（开发树是 `target\release\voice-core.exe`），
数据目录是安装根下的 `data\`；安装在不可写目录（Program Files）时 runtime 会退到 `%APPDATA%\voice-core`。

## 1. 两段文本和它们的对齐

- `text` 是**念出来的文本**，`displayText` 是**给人看的文本**，两者都由你产出——翻译是你的责任，
  runtime 从不翻译。`text` 是 API 层唯一必填的字段，但音色包在引擎层面也是必需的（§2）。
  当前唯一后端 Irodori 最擅长日语，所以今天的实际写法是
  `text` 日文、`displayText` 中文。
- `rubyPairs` 是两串之间的**分段对应**：`base` 是 `displayText` 的片段，`ruby` 是 `text` 里对应它的片段，
  按阅读顺序，两侧各自拼接必须还原完整原串。中日文不按位置对齐（动词一个在中间一个在句尾，
  翻译还会合并/拆分/重排子句），下游推不出来，只有产出两串的你知道。不给也能工作，
  字幕会退化成整行旁注。标点照样成对给（`{"base":"，","ruby":"、"}`）：拼接规则要求它们在，
  渲染端自己会丢掉纯标点的旁注。

CLI 传法（`@pairs.json` 可以从文件读同一个数组，长句省事）：

```powershell
--ruby-pairs '[{"base":"欢迎回来","ruby":"おかえりなさい"},{"base":"，","ruby":"、"},{"base":"老师。","ruby":"先生。"}]'
```

## 2. 音色包：怎么列、怎么选

```powershell
E:\NewToolBox\voice-core\bin\voice-core.exe voices
```

真实输出，三列 = `id` / `name` / `kind`：

```
ba-miyu-lora         ba-miyu-lora             lora-adapter
ba-shun-kid-lora     ba-shun-kid-lora         lora-adapter
```

- `--voice`（HTTP 里是 `voicePackId`）取**第一列的 id**，不是 name。
- 一个包都没装时这条命令打印 `no voice packs registered in config.json`。
- **`--voice` 实际上是必填的。** API 层只校验 `text`，但引擎是从音色包合成的：不带包发过去会拿到
  `[internal] engine could not synthesize this utterance: Specify ref_wav/ref_wavs/ref_latent/ref_latents, or set no_ref=True.`
  （实测）。永远显式选一个 id。
- 想看 `languages` / `character` / `avatar` / `path` 得走 `GET /api/voices`，CLI 只印三列。
  发之前对一下语言：现有包都是 `["ja"]`，把中文塞给它只会得到听不懂的音频而不是报错。
- 用户要一个列表里没有的音色：如实说没有，指向 §6，**不要编造 id**。

## 3. HTTP 接口

Base 默认 `http://127.0.0.1:8760`（runtime `--bind` 可以改，但它只有 bearer token、没有 TLS，
所以只应该绑回环）。鉴权是 `Authorization: Bearer <token>`，token 的值在**数据目录的 `token.txt`**
（本机 `E:\NewToolBox\voice-core\data\token.txt`，runtime 首次启动自建）。每次从文件读；
**不要把它写进代码、日志、提交、报告或任何会外发的内容**。`GET /api/health` 是唯一免鉴权路由，其余全部 401。

| 方法 | 路由 | 用途 |
|---|---|---|
| GET | `/api/health` | 存活探测，不需要 token：`{"name","runtimeVersion","apiVersion","ready"}` |
| GET | `/api/status` | runtime / 引擎 / spool / presenter / 在飞请求 |
| GET | `/api/metrics` | 计数器与合成延迟分位 |
| GET | `/api/voices` | 已装音色包数组 |
| POST | `/api/speak` | 合成一句 |
| GET | `/api/audio/{audioId}` | 取 WAV 字节（`audio/wav`，也答 `HEAD`） |
| GET | `/api/events` | SSE：字幕、引擎状态、进度、失败 |
| POST | `/api/warm` | 现在就加载模型，`modelLoaded` 为真才返回 |
| POST | `/api/sleep` | 停引擎进程放显存，runtime 继续服务 |
| DELETE | `/api/requests/{requestId}` | 取消（放开调用方，但引擎跑完当前 step 才放 GPU） |
| POST | `/api/shutdown` | 让 runtime 退出 |

`POST /api/speak` 请求字段：`text`（必填）、`voicePackId`（引擎层面也必填，见 §2）、`displayText`、
`rubyPairs`、`seed`、`numSteps`（默认 32）、`displaySeconds`（字幕停留秒数）、`timeoutMs`。
响应：`requestId`、`audioId`、`sampleRate`、`durationMs`、`bytes`、`displayText`、`voicePackId`、
`presenters`、`coldStart`、`queueMs`、`synthMs`、`totalMs`。**音频不在 JSON 里**，字节走
`GET /api/audio/{audioId}`；全系统没有 base64。`audioId` 只在 spool 里有效（默认 TTL 3600 s、
总量上限 2048 MB、runtime 重启即清），过期是 `not_found`，重新合成即可。

curl（bash / git-bash）：

```bash
VC=/e/NewToolBox/voice-core
curl -s -X POST http://127.0.0.1:8760/api/speak \
  -H "Authorization: Bearer $(cat "$VC/data/token.txt")" \
  -H 'Content-Type: application/json' \
  -d '{"text":"おかえりなさい、先生。",
       "displayText":"欢迎回来，老师。",
       "rubyPairs":[{"base":"欢迎回来","ruby":"おかえりなさい"},
                    {"base":"，","ruby":"、"},
                    {"base":"老师。","ruby":"先生。"}],
       "voicePackId":"ba-miyu-lora"}'
```

PowerShell（`-ContentType` 必须带 `charset=utf-8`，否则 PS 5.1 会静默把日文发成乱码：
服务端不报错，只是念出错的东西——实测同一句话 durationMs 3120 → 3320）：

```powershell
$root  = 'E:\NewToolBox\voice-core'
$token = (Get-Content "$root\data\token.txt" -Raw).Trim()
$body  = @{
  text        = 'おかえりなさい、先生。'
  displayText = '欢迎回来，老师。'
  voicePackId = 'ba-miyu-lora'
} | ConvertTo-Json -Depth 5
$r = Invoke-RestMethod -Method Post -Uri 'http://127.0.0.1:8760/api/speak' `
  -Headers @{ Authorization = "Bearer $token" } `
  -ContentType 'application/json; charset=utf-8' `
  -Body ([Text.Encoding]::UTF8.GetBytes($body)) -TimeoutSec 300
"$($r.requestId) $($r.totalMs)ms cold=$($r.coldStart) presenters=$($r.presenters)"
```

播放和字幕不是你的事：字幕进程订阅 `/api/events`，自己播音频、自己弹对话框。它由根目录的 GUI
（`VoiceCore.exe`；1.1.0 安装是 `bin\app\VoiceCoreTray.exe`）拉起，agent 不要去启动它。
`presenters` 就是当前订阅数：为 0 说明没人在听——CLI 的 `--play auto`（默认）正是靠这个决定要不要自己播，
`--play always` 强制自己播，`--play never` 不播——且不带 `--out` 时它连音频字节都不取，
要留 WAV 就给 `--out <path>`。
你自己订阅 `/api/events` 期间也算一个 presenter，会让 `--play auto` 不再播。

事件流每帧一个 JSON 信封（`seq`、`tsMs`、`kind` + 该 kind 的字段），新订阅者先收到最近 64 条尾巴。
`kind` ∈ `runtimeReady` / `runtimeStopping` / `workerStarting` / `workerReady` / `workerStopped` /
`speakStarted` / `speech` / `speakFailed` / `progress`。字幕和播放需要的一切都在 `speech` 一帧里
（`audioId`、`text`、`displayText`、`rubyPairs`、`voicePackId`、`durationMs`、`sampleRate`、`displaySeconds`）。
runtime 从不反向调用前端，这是唯一的推送通道。

## 4. 失败时的自诊断

`voice-core.exe doctor` 一次覆盖下面第 1、2、4、5 条（可达性 / 鉴权 / 引擎 / 包数量），
但它**永远退出 0**，要读输出而不是退出码：

```
runtime      reachable, api v1
token        accepted
engine       managed=true running=false model_loaded=false idle=4379035ms
voice packs  2
presenters   0
spool        0 entr(ies), 0 bytes
```

`engine running=false` 不是故障：空闲久了 runtime 会主动停引擎，下一句自己拉起来（就是冷启动）。

| 判据 | 结论 | 动作 |
|---|---|---|
| `GET /api/health` 连不上 | 服务没起 | 让用户启动根目录的 GUI（同时拉起服务和字幕）；只要声音不要字幕时可 `Start-Process -WindowStyle Hidden "E:\NewToolBox\voice-core\bin\voice-core-runtime.exe"`（无参数即可，引擎路径来自 `data\runtime.json`），然后轮询 `/api/health` 到 200——启动不加载模型，是毫秒级 |
| runtime 立刻退出，`data\logs\runtime.err.log` 里有 `no TTS engine configured` | 引擎没配置 | 见下一行，这是部署问题不是调用问题 |
| `data\logs\runtime.err.log` 里有 `cannot bind 127.0.0.1:8760; is another runtime already running?` | 端口被占 | 已经有一个实例在跑（第二个在碰 spool 之前就退出了，不会清掉在跑那个的音频）。别再启动，直接 `status` |
| health 200，但任何受保护路由 401 | token 不对 / 数据目录不一致 | 从 runtime 自己报的数据目录读 `token.txt`（`--print-layout` 会打印 data dir）；用 CLI 就不用管这件事 |
| `status.worker.missing` 非空，或 speak 报 `worker_start_failed` | 环境没部署好 | `voice-core-runtime.exe --print-layout` 会逐项打 `ok` / `MISSING`（解释器、worker 脚本、引擎根、HF 缓存）。缺东西交给用户在 GUI 里部署，**agent 不要自己去下 4.8 GB 模型** |
| `status.voicePacks` 为 0，或 speak 报 `voice_pack_not_found` | 音色包没注册 | `voices` 看真实列表；`recovery.detail` 里就有已装 id。如实告知，不要编造 |
| speak 报 `model_load_failed` / `internal` | 引擎起来了但这句失败了 | 读 `data\logs\tts-worker.err.log`（消息里已带引擎自己的原因） |
| 用户说"听不到" | 大概率没人在播 | `status.presenters` 为 0 就是没有字幕进程在听：让用户启动 GUI，或你自己用 `--play always` |

`--print-layout` 真实输出（本机，不启动任何东西；引擎那几行的路径前缀已截短）：

```
install root   E:\NewToolBox\voice-core
data dir       E:\NewToolBox\voice-core\data
packs          E:\NewToolBox\voice-core\data\config.json
interpreter    ok      ...\irodori-tts\env\Scripts\python.exe
worker script  ok      E:\NewToolBox\voice-core\runtime/worker/irodori/worker.py
engine root    ok      ...\irodori-tts
HF_HOME        ok      ...\irodori-tts\model\huggingface
```

## 5. 错误码 → 动作

每个非 2xx 都是同一形状，**按 `code` 分支，不要看文案**：

```json
{"code":"unauthorized","message":"missing or invalid bearer token",
 "recovery":{"kind":"check_token","detail":"read token.txt from the data dir"}}
```

| `code` | HTTP | 你的动作 |
|---|---|---|
| `unauthorized` | 401 | 重读 `token.txt`；health 通就是数据目录不一致 |
| `invalid_request` | 400 | `text` 为空或请求体不合法，改请求 |
| `voice_pack_not_found` | 404 | `recovery.detail` 列出已装 id，如实告知 |
| `not_found` | 404 | `audioId` 过期或不存在，重新合成 |
| `worker_unavailable` | 503 | 外挂引擎没应答，或引擎在请求中途死了；重试一次 |
| `worker_start_failed` | 500 | 引擎起不来；消息带实际等待毫秒和 stderr 尾巴，读 `tts-worker.err.log` |
| `model_load_failed` | 500 | 引擎活着但模型没加载起来；消息里有引擎自己的原因 |
| `resource_busy` | 429 | 排队等设备超过 `timeoutMs`，引擎根本没看到这句；退避重试 |
| `deadline_exceeded` | 504 | 合成超过 `timeoutMs`；调大重试一次（冷启动最容易撞这条） |
| `cancelled` | 499 | 调用方自己取消的 |
| `internal` | 500 | 其他；消息带引擎原文，`recovery` 指向引擎日志 |

`recovery.kind` ∈ `retry` / `wait` / `check_token` / `check_worker_logs` / `install_voice_pack` / `fix_request`。

合成是串行的：一次一句。第二句会排队，等到 `timeoutMs` 还没轮到就是 `resource_busy`，
所以别并发发 speak。`timeoutMs` 分别约束排队和合成两段，最坏墙钟接近它的两倍。

日志与指标：`data\logs\runtime.{out,err}.log`、`data\logs\tts-worker.{out,err}.log`、
`data\logs\dialog.jsonl`（字幕每句一行）、`data\metrics.jsonl`（每次合成一行延迟）。

## 6. 新音色包：注册与训练

三种 `kind`：

| `kind` | 产物 | `path` 指向 |
|---|---|---|
| `lora-adapter` | **目录**，含 `adapter_config.json` + `adapter_model.safetensors` | 该目录 |
| `speaker-embedding` | **单文件，文件名必须以 `.speaker.safetensors` 结尾** | 该文件 |
| `reference-audio` | 参考音频 | 音频文件 |

改名踩过的坑：把 SE 文件改成没有后缀的名字会被引擎按名字拒绝
（`Speaker Inversion embeddings must use the '.speaker.safetensors' suffix`），拷进 `data\voicepacks\` 时保留原文件名。

注册有两个文件，各管一件事（下面就是完整规则，不需要别的文档）：

**1. 包自己描述自己** —— 目录包写 `<包目录>/voicepack.json`，单文件包写同目录的 `<stem>.voicepack.json`：

```jsonc
{
  "schema": 1,                       // 唯一必填
  "name": "霞沢美游 (LoRA)",
  "engine": "irodori-tts-v4.1-small",
  "kind": "lora-adapter",            // 省了就按载荷推断
  "languages": ["ja"],
  "character": "霞沢美游",            // 字幕里显示的说话人，缺省回落到 name
  "avatar": "avatar.png"             // 相对**包自己**，所以拷到别的机器不丢脸
}
```

**2. `data\config.json` 只说装了哪些包、在哪** —— `voicePacks` 数组里一条指针就够
（JSONC：允许 `//` 注释、尾随逗号、BOM）：

```jsonc
{
  "id": "ba-miyu-lora",              // speak --voice / voicePackId 用的就是它
  "path": "voicepacks/ba-miyu-lora"  // 相对数据目录（便携）或绝对路径
}
```

两边写了同一个字段时**包内的赢**：条目是生成物（安装器铺的、面板写的），不该盖过包自己的说法。
所以要改一个包的名字或头像，改包内的 `voicepack.json`，别改条目。没有清单的包完全合法，信息全取自条目。

runtime 按 mtime 自动重载这一段，不用重启；写完 `voices` 立刻能列出来。
训练脚本在安装树的 `scripts/training/`（`install_pack.py --help` 说明每个参数）；
完整训练流程是开发机上的文档，不随安装分发。agent 需要记住的四条：

- 音色包**只用 LoRA**。参考音频（`reference-audio`）仍然能注册，适合"只要音色、不要风格"；
  Speaker Inversion 那条路已经停做。
- LoRA 要音频 + **与音频同语言**的逐条转写，约 60–70 条 / 总时长 ~15 分钟够用。中文转写配日语音频会让
  adapter 学到生成时不成立的文本域映射（踩过）。
- 选 checkpoint 看 val loss，不要拿最后一步（本例 1000 步优于 2000 步）。
- 验收看相似度分布的下限，不看均值（本例均值 0.771 / p10 0.703）。

## 7. 不要做的事

- **不要直连 Python worker。** `status.worker.port` 是每次启动重新分配的临时端口，只有 runtime 该碰它；
  所有调用走 `127.0.0.1:8760`。
- **不要绕过 runtime 改配置。** `data\config.json` 里只有 `voicePacks` 是 runtime 读的（热重载），
  `dialog` / `hotkeys` 属于字幕进程、改完要重启它才生效；`data\runtime.json`（引擎与模型路径）是部署产物，
  改了不重启 runtime 也不会读，重启会打断用户。要装音色包就只**追加** `voicePacks` 条目、且先征得用户同意，
  文件里的注释和其他段落一个字都不要动。
- **不要假设模型路径。** 解释器、引擎根、HF 缓存每台机器都不一样，来自 `data\runtime.json`；
  要知道就跑 `--print-layout`，不要硬编码 `HF_HOME` 或权重目录，也不要自己去下模型。
- **不要把 token 写进任何会外发的内容**，包括你的回复、日志和提交。
- **不要并发 speak**，也不要把超时设成几秒——见 §0 的冷启动。
- runtime 不做 LLM 推理、不做对话管理、不做翻译，也不做语音识别。要说什么、翻成什么，都是你的责任。
