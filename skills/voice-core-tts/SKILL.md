---
name: voice-core-tts
description: 用本机 voice-core runtime 让 agent 真的出声：合成一句语音、自动播放、弹出字幕对话框，可指定角色音色包。当任务涉及本机语音合成 / TTS / 让 agent 说话或念一段话 / 连续念多段旁白 / 用某个角色的声音回复 / 音色包怎么选 / 停顿与表情 / 字幕弹窗 / voice-core 这个产品本身时使用。内容是最短调用路径、怎么等一段真的放完（`--wait`）、停顿怎么写（`[pause:N]`）、音色包怎么列怎么选、错误码怎么处置、失败怎么自诊断。
---

# voice-core-tts：让 agent 在这台机器上真的出声

一次调用 = 合成一句 → 播放 → 弹字幕。模型、Python 解释器、引擎端口、显存回收都封在 runtime 里。

**用哪个接口，不是偏好问题：**

- **要说一句话 → 用 CLI** `bin\voice-core.exe`。它自己找数据目录、自己读 token、自己决定要不要放音，
  一行命令就是完整的一次调用；出错时 stderr 直接给 `[错误码] 原因 | try: 建议`。日常让 agent 出声只用这个。
- **要写一个跑在 voice-core-runtime 上的程序 → 才用 HTTP**（§3）。需要流式订阅事件、自己管理并发和取消、
  自己拿 `audioId` 取音频字节、或者把 runtime 当服务集成进别的进程时，HTTP 才是对的层。

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
模型常驻后一句约 0.6 秒（实测 p50 636 ms / p95 701 ms；32 步、模型常驻、机器空闲。本页更早的
几段 `实测` 输出测于引擎的调度修复与 CUDA graph 之前，那里的合成耗时会高出数倍）。
**0.6 秒是最好情况**：它测在 CUDA graph 捕获生效时（默认开，峰值约需 3.2 GiB 显存）。显存不够时捕获会
失败，引擎在日志里说一次然后改用逐步采样——音频逐字节相同，但一句约 2.5 秒。所以**看到 2.5 秒不是卡住**，
去 `data\logs\tts-worker.err.log` 找那一行；`VC_ENGINE_CUDA_GRAPHS=0` 就是显式选这个较慢的 regime。
空闲 15 分钟后 runtime 自己卸模型、空闲 60 分钟后停引擎进程，
所以隔一阵子的第一句又是冷启动。**客户端超时至少给 120 s**，别按"HTTP 一秒该返回"设计；
CLI 自己等 `--timeout-ms / 1000 + 30` 秒（默认 630 s），服务端合成上限默认 600 s。
要把这几十秒挪到用户不在等的时候：先 `voice-core.exe warm`（或 `POST /api/warm`），
它加载完模型才返回，之后的第一句就是热的。

**要按顺序念好几段（旁白、多轮对话）就加 `--wait`。** 不加时 speak 在音频**生成好**的那一刻就返回，
不是**放完**的那一刻：三段连着发会同时开口，而 `time.sleep(7.5)` 是猜的、两个方向都会猜错。
加上 `--wait` 它先订阅事件流再发请求，等到「这段真的放完了」那一帧才返回——字幕进程放的也算，
它自己放的也算，你不需要知道是谁放的：

```powershell
$vc = 'E:\NewToolBox\voice-core\bin\voice-core.exe'
& $vc speak --voice ba-miyu-lora --text "おかえりなさい、先生。"     --display "欢迎回来，老师。"   --wait
& $vc speak --voice ba-miyu-lora --text "今日はいい天気ですね、先生。" --display "今天天气很好呢，老师。" --wait
```

实测第二句（字幕进程在跑，由它放音）：

```
今天天气很好呢，老师。
request 1ae88bc9c7a646b4 | 7823 ms total (3403 ms synth) | 1 presenter(s)
```

`--wait` 时 `total` 是**含播放的墙钟**（7823 ms），合成只占其中 3403 ms；不加 `--wait` 时它还是
runtime 报的合成端到端时间，和以前一样。没有任何前端放音时，`--wait` 等到 `durationMs + 5 s`
就**退 1** 并说清楚没等到什么（`--wait-timeout-ms <ms>` 改这个预算）：

```
Error: --wait: nothing reported audio 46e78777... finished playing within 9320 ms;
no frontend reported playing it at all: start the presenter, or pass --play always to play it here
```

所以 `--play never --wait` 是自相矛盾的写法。没有字幕进程时用默认的 `--play auto` 就好：
CLI 自己放、自己上报，`--wait` 一样收得到闭环（实测 `request ec5b1b6b... | 6781 ms total (3250 ms synth)`）。

`--play auto`（默认）是按 runtime 报的当前字幕订阅数决定的：有字幕进程在听就让它放，没有就自己放；
`--play always` 强制自己播，`--play never` 不播——且不带 `--out` 时它连音频字节都不取，
要留 WAV 就给 `--out <path>`。字幕窗口由根目录的 GUI（`VoiceCore.exe`）拉起，**agent 不要去启动它**。

退出码只有三个：`0` 成功，`1` 调用失败（stderr 形如 `[voice_pack_not_found] ... try: ...`），
`2` 参数写错。唯一例外是 `doctor`：它永远退 0，判断要读输出。

不在这台机器上：CLI 一律是安装根下的 `bin\voice-core.exe`（开发树是 `target\release\voice-core.exe`），
数据目录是安装根下的 `data\`；安装在不可写目录（Program Files）时 runtime 会退到 `%APPDATA%\voice-core`。

## 1. 两段文本、对齐、停顿、语言、表情

- `text` 是**念出来的文本**，`displayText` 是**给人看的文本**，两者都由你产出——翻译是你的责任，
  runtime 从不翻译。`text` 是 API 层唯一必填的字段，但音色包在引擎层面也是必需的（§2）。
  当前唯一后端 Irodori 最擅长日语，所以今天的实际写法是
  `text` 日文、`displayText` 中文。
- `rubyPairs` 是两串之间的**分段对应**：`base` 是 `displayText` 的片段，`ruby` 是 `text` 里对应它的片段，
  按阅读顺序，两侧各自拼接必须还原完整原串。中日文不按位置对齐（动词一个在中间一个在句尾，
  翻译还会合并/拆分/重排子句），下游推不出来，只有产出两串的你知道。不给也能工作，
  字幕会退化成整行旁注。标点照样成对给（`{"base":"，","ruby":"、"}`）：拼接规则要求它们在，
  渲染端自己会丢掉纯标点的旁注。
- **拼接对不上不再默默降级。** `rubyPairs` 还原不出 `text` 或 `displayText` 时是 `invalid_request`，
  并指出**第一个对不上的下标**、已经拼到第几个字、以及两边分别是什么——一眼就能改：

  ```
  [invalid_request] rubyPairs[2].ruby does not line up with text: the first 2 pair(s)
  reconstructed 8 character(s), then this one offers `せんせい。` where text has `先生。`
  ```

  这是调用方的 bug，只有你能修（下游没有任何东西推得出正确的对齐），所以它现在会响。
  空数组等于没给，不算对不上。
- **停顿只有一种写法：`[pause:N]`，N = 毫秒，1–10000，直接写在 `text` 里。**
  runtime 在标记处把这句切开、分段合成、再把 N 毫秒静音拼回去：**一个 `audioId`、一帧 `speech`、
  `durationMs` 把静音算进去**，所以 `--wait` 和字幕停留时间都自动是对的。标记不会被念出来，
  事件里的 `text` 是去掉标记后的那句（`rubyPairs` 也是按去掉标记的文本对齐，切分不会重排你的数组）。
  相邻两个标记相加；写在整句最前或最后的标记会被丢掉，并在事件流里说一声（句首句尾的停顿是你自己的等待）；
  写坏了（`[pause:abc]`、`[pause:99999]`）是 `invalid_request` 并把那段原文贴回给你，**绝不当字面文本念出来**。

  ```powershell
  --text "おかえりなさい、先生。[pause:600]今日はいい天気ですね。"
  ```

  实测：整句不带标记 5760 ms，带 `[pause:600]` 是 6880 ms，其中**正好 600 ms** 是拼进去的静音
  （两半各自单独合成是 3120 + 3160 ms），多出来的 520 ms 是分段各自的起音和收尾。
  所以标记是给你真正想要的那个气口用的，不要拿它当省时间的手段，也不要一句里撒十个。
- `language`（CLI `--language`）可选，短 BCP-47 标签（`ja`、`zh-CN`、`en-US`）。写了就会校验：
  包声明的 `languages` 不包含它，直接 `voice_language_unsupported`，消息里带包 id、它声明的、你要的；
  不写就和以前完全一样（runtime 不检测、不猜、不翻译）。标签不分大小写、按子标签比：`ja-JP` 满足声明 `ja` 的包，
  `zh-TW` 不满足声明 `zh-CN` 的包；一个语言都没声明的包谁都不冲突，放行。
  **它只做校验，不做引擎路由**——第二个引擎是以后的事，现在先有字段、校验和错误码，你可以照着写。
  现有包都是 `["ja"]`，所以 `--language zh-CN` 会被拒，而这正是过去把中文塞进日语模型、
  拿到一段听不懂的音频却没有任何报错的那个坑。

CLI 传法（`@pairs.json` 从文件读同一个数组；`-` 从 stdin 读——长数组和 PowerShell 引号打架时用它最稳，
Windows 引号问题就此消失）：

```powershell
--ruby-pairs '[{"base":"欢迎回来","ruby":"おかえりなさい"},{"base":"，","ruby":"、"},{"base":"老师。","ruby":"先生。"}]'
```

```powershell
$pairs = '[{"base":"欢迎回来","ruby":"おかえりなさい"},{"base":"老师。","ruby":"先生。"}]'
$pairs | & $vc speak --voice ba-miyu-lora --text "おかえりなさい、先生。" --display "欢迎回来，老师。" --ruby-pairs -
```

### 表情：两条通道，都能用

这个模型是**带表情条件**训练的，两条路都不需要改代码、不需要再训练：

**A. 直接把 emoji 写进 `--text` 里。** 它们不会被念出来，只改变念法；同一个重复写就更强。

```powershell
& $vc speak --voice ba-miyu-lora --text "😭😭おかえりなさい、先生。" --display "欢迎回来，老师。"
```

**`--display` 里不要写 emoji**——那一行是给人看的字幕，`--text` 才是给模型的。

**B. 用 `--emotion` 走 caption 通道**，它是和文字分开的一路条件，所以整句的“怎么说”不必混进
要念的字里。可以写自然语言，也可以写 emoji：

```powershell
& $vc speak --voice ba-miyu-lora --text "おかえりなさい、先生。" --emotion "泣きながら、震える声で"
& $vc speak --voice ba-miyu-lora --text "おかえりなさい、先生。" --emotion "😱😱" --cfg-scale-caption 5
```

`--cfg-scale-caption` 是这条通道的引导强度，`0..=10`，引擎默认 3.0；写超了是
`invalid_request`（**不会**给你悄悄夹到边界），没有 caption 却给了它也是 `invalid_request`。

实测（同一句、同一个包、`--seed 1234`、`--steps 32`）：不给表情 `durationMs` 3120，
`--emotion "泣きながら、震える声で"` 是 2920，`--text "😭😭…"` 是 3880，三段音频的 sha256 各不相同；
**什么都不给时，音频和加这个通道之前的版本逐字节相同**。

**C. 让包自己带。** 包清单里写 `"expression": { "emotion": "🫶🫶", "cfgScaleCaption": 3.0 }`，
这个角色以后就默认这么说话，调用方一个字都不用多写。
单次的 `--emotion` 压过包里的；`--emotion ""`（显式空串）= 这一句不要表情，也不回落到包里的那个。

#### 45 个 emoji（照抄 checkpoint 自带的 `EMOJI_ANNOTATIONS.md`，不要自己编）

| emoji | 意思 | emoji | 意思 | emoji | 意思 |
|---|---|---|---|---|---|
| 👂 | 囁き、耳元の音 | 😮‍💨 | 吐息、溜息、寝息 | ⏸️ | 間、沈黙 |
| 🤭 | 笑い（くすくす、含み笑い） | 🥵 | 喘ぎ、うめき声、唸り声 | 📢 | エコー、リバーブ |
| 😏 | からかうように、甘えるように | 🥺 | 声を震わせて、自信なさげに | 🌬️ | 息切れ、荒い息遣い |
| 😮 | 息をのむ | 👅 | 舐める音、咀嚼音、水音 | 💋 | リップノイズ |
| 🫶 | 優しく | 😭 | 嗚咽、泣き声、悲しみ | 😱 | 悲鳴、叫び、絶叫 |
| 😪 | 眠そうに、気だるげに | 😴 | 寝言、いびき | ⏩ | 早口、まくしたてる、急いで |
| 📞 | 電話越し、スピーカー越し | 🐢 | ゆっくりと | 🥤 | 唾を飲み込む音 |
| 🤧 | 咳き込み、鼻をすする、くしゃみ | 😒 | 舌打ち | 😰 | 慌てて、動揺、緊張、どもり |
| 😆 | 喜びながら | 💥 | 勢いよく | 😠 | 怒り、不満げに、拗ねながら |
| 😲 | 驚き、感嘆 | 🥱 | あくび | 😖 | 苦しげに |
| 😟 | 心配そうに | 🫣 | 恥ずかしそうに、照れながら | 🙄 | 呆れたように |
| 😊 | 楽しげに、嬉しそうに | 😎 | 得意げに、自信ありげに | 👌 | 相槌、頷く音 |
| 🙏 | 懇願するように | 🥴 | 酔っ払って | 🎵 | 鼻歌 |
| 🤐 | 口を塞がれて | 😌 | 安堵、満足げに | 🤔 | 疑問の声 |
| 💪 | 力を込めて、力強く | 👃 | 匂いを嗅ぐ音 | 📖 | ナレーション、独白 |

源文件在 checkpoint 快照里（`models--Aratako--Irodori-TTS-v4.1-Small/snapshots/*/EMOJI_ANNOTATIONS.md`），
它自己也说了：**同一个 emoji 重复写可以加强效果**，控制不是百分之百精确，值得多试两次。

### 字幕外观：这一句想不一样就单次覆盖

`--name-color` / `--text-color` / `--ruby-color` / `--countdown-color`（`#rgb`、`#rrggbb`、
`#aarrggbb`）、`--reveal`（只有 `typewriter` | `sweep` | `fade`）、`--display-seconds`。
优先级：**单次调用 > 包清单 `dialog` > `config.json` 的 `dialog` 节 > 字幕端内置值**，逐字段生效。
颜色写错、`reveal` 写了不存在的值，都是 `invalid_request` 并点名那个字段，不会被默默忽略。

平时不用管这些：把配色写进包清单，换个音色包字幕就自动换一身衣服，回看历史也是按当句的包上妆。

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
  现有包都是 `["ja"]`。要让 runtime 帮你挡住语言错配，就在 speak 里带上 `language`（§1）：
  它会直接报 `voice_language_unsupported`，而不是念出一段听不懂的音频。
- 用户要一个列表里没有的音色：如实说没有，**不要编造 id**。真的要做一个新的，那是另一件事，
  用 `voice-core-voice-training` 技能（训练一个 LoRA 音色包，一次 50–90 分钟的 GPU 作业）。

## 3. 要写集成才用 HTTP

Base 默认 `http://127.0.0.1:8760`（runtime `--bind` 可以改，但它只有 bearer token、没有 TLS，
所以只应该绑回环）。鉴权是 `Authorization: Bearer <token>`，token 的值在**数据目录的 `token.txt`**
（本机 `E:\NewToolBox\voice-core\data\token.txt`，runtime 首次启动自建）。每次从文件读；
**不要把它写进代码、日志、提交、报告或任何会外发的内容**。`GET /api/health` 是唯一免鉴权路由，其余全部 401。

路由表、`POST /api/speak` 的每个请求与响应字段、事件流每个 `kind` 的形状、`POST /api/played` 的播放闭环、
取消与空闲回收的语义——**完整契约在 `E:\NewToolBox\voice-core\docs\api.md`**（`voice-core API v1`），
它和 runtime 一起装在这台机器上。要写集成就读那一份，本页不复述：同一件事写两遍，迟早有一遍是错的。

两件在那份文档里、但值得在这里点名的事：**音频从不走 JSON**（`speak` 给 `audioId`，字节走
`GET /api/audio/{audioId}`，全系统没有 base64），**runtime 从不反向调用前端**（前端订阅
`GET /api/events`，放完了自己 `POST /api/played` 往里报）。你自己订阅 `/api/events` 期间也算一个
presenter，会让 CLI 的 `--play auto` 不再播。

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

每个失败都是同一形状，**按 `code` 分支，不要看文案**（CLI 把它印成 `[code] message | try: ...`；
HTTP 的状态码与响应体形状见 `docs\api.md` 的 `Errors` 节）：

```json
{"code":"unauthorized","message":"missing or invalid bearer token",
 "recovery":{"kind":"check_token","detail":"read token.txt from the data dir"}}
```

| `code` | 你的动作 |
|---|---|
| `unauthorized` | 重读 `token.txt`；health 通就是数据目录不一致 |
| `invalid_request` | `text` 为空、`[pause:N]` 写坏了、或 `rubyPairs` 拼不回原串；消息里带出错的那一段，改请求 |
| `voice_pack_not_found` | `recovery.detail` 列出已装 id，如实告知 |
| `voice_language_unsupported` | 这个包不声明你要的语言（消息里带包 id、它声明的、你要的）：换包、换 `language`、或不带 `language` |
| `not_found` | `audioId` 过期或不存在，重新合成（`/api/played` 报一个不存在的 id 也是这条） |
| `worker_unavailable` | 外挂引擎没应答，或引擎在请求中途死了；重试一次 |
| `worker_start_failed` | 引擎起不来；消息带实际等待毫秒和 stderr 尾巴，读 `tts-worker.err.log` |
| `model_load_failed` | 引擎活着但模型没加载起来；消息里有引擎自己的原因 |
| `resource_busy` | 排队等设备超过 `timeoutMs`，引擎根本没看到这句；退避重试 |
| `deadline_exceeded` | 合成超过 `timeoutMs`；调大重试一次（冷启动最容易撞这条） |
| `cancelled` | 调用方自己取消的 |
| `internal` | 其他；消息带引擎原文，`recovery` 指向引擎日志 |

`recovery.kind` ∈ `retry` / `wait` / `check_token` / `check_worker_logs` / `install_voice_pack` / `fix_request`。

合成是串行的：一次一句。第二句会排队，等到 `timeoutMs` 还没轮到就是 `resource_busy`，
所以别并发发 speak。`timeoutMs` 分别约束排队和合成两段，最坏墙钟接近它的两倍。

日志与指标：`data\logs\runtime.{out,err}.log`、`data\logs\tts-worker.{out,err}.log`、
`data\logs\dialog.jsonl`（字幕每句一行）、`data\metrics.jsonl`（每次合成一行延迟）。

## 6. 不要做的事

- **不要直连 Python worker。** `status.worker.port` 是每次启动重新分配的临时端口，只有 runtime 该碰它；
  所有调用走 `127.0.0.1:8760`。
- **不要绕过 runtime 改配置。** `data\config.json` 的 `voicePacks` 与 `dialog` 由 runtime 按 mtime 热重载，
  `dialog` / `hotkeys` 由字幕进程同样按 mtime 热重载——**都改完立刻生效，不用重启任何东西**；
  `data\runtime.json`（引擎与模型路径）是部署产物，改了不重启 runtime 也不会读，重启会打断用户。
  要装音色包就只**追加** `voicePacks` 条目、且先征得用户同意，
  文件里的注释和其他段落一个字都不要动。
- **不要假设模型路径。** 解释器、引擎根、HF 缓存每台机器都不一样，来自 `data\runtime.json`；
  要知道就跑 `--print-layout`，不要硬编码 `HF_HOME` 或权重目录，也不要自己去下模型。
- **不要把 token 写进任何会外发的内容**，包括你的回复、日志和提交。
- **不要并发 speak**，也不要把超时设成几秒——见 §0 的冷启动。
- **不要用 `sleep` 猜播放时长。** 要按顺序念多段就 `--wait`（§0），或者自己订阅 `/api/events`
  等 `playbackFinished`；猜出来的等待要么抢话，要么白等。
- **不要写 `[pause:N]` 以外的任何标记。** 没有 SSML、没有 `<break>`、没有情感标签——
  runtime 只认 `[pause:N]`，别的会原样交给引擎**当字面文本念出来**。
- runtime 不做 LLM 推理、不做对话管理、不做翻译，也不做语音识别。要说什么、翻成什么，都是你的责任。
