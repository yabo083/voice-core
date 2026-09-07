---
name: voice-core-tts
description: 用 voice-core runtime 让 agent 出声：合成语音、自动播放、弹出字幕对话框，支持多角色音色包。用于语音交互、旁白朗读、多角色配音及字幕控制。
---

# voice-core-tts：语音合成与出声

单次调用完成：TTS 合成 → 本地音频播放 → WinUI3 悬浮字幕弹窗。

- **Agent 出声唯一推荐**：CLI `voice-core`（可执行文件位于安装根目录下的 `bin/voice-core.exe`，自动管理数据目录、Token、播放与字幕同步）。
- **程序化集成才用 HTTP**：`http://127.0.0.1:8760`（Bearer Token 读取自数据目录下的 `token.txt`，详见 `docs/api.md`）。

---

## 1. 快速调用范式

安装根目录记为 `<install-root>`，CLI 路径为 `<install-root>/bin/voice-core.exe`。

### 基础单句调用
```powershell
$vc = '<install-root>\bin\voice-core.exe'
& $vc speak --voice sora-ikaros-lora `
  --text "マスター、何でしょうか？" `
  --display "Master，请问有什么吩咐吗？"
```

### 多句顺序连续调用（必须加 `--wait`）
不加 `--wait` 会在音频**生成完成**时立即返回，导致多句并发重叠播放；加 `--wait` 会订阅事件流并在**放完最后一帧**后才返回：
```powershell
& $vc speak --voice sora-ikaros-lora --text "了解いたしました。[pause:300]マスター。" --display "收到，Master。" --wait
& $vc speak --voice sora-ikaros-lora --text "直ちに実行します。" --display "立刻执行。" --wait
```

- `--play auto`（默认）：有字幕前端在线由前端放音，无字幕前端由 CLI 自己放音。
- 冷启动耗时约 15–30 秒（加载模型），模型驻留显存后每句约 0.6–1.5 秒。推理显存峰值约 2.4–3.2 GiB。
- 欲免除用户等待冷启动：提前执行 `& $vc warm`。

---

## 2. 文本格式、停顿与行内表情控制

### 字段规范
- `--text`：**念出来的文本**（当前 Irodori 引擎推荐输入日文）。
- `--display`：**人类阅读的字幕文本**（显示在字幕窗口中，不输入模型，禁止写 Emoji）。
- `[pause:N]`：**精准静音停顿**（毫秒，1–10000），直接写在 `--text` 中。引擎自动拼接静音切片并同步计算字幕停留时长。

### Emoji 语气控制（必须行内句中精准嵌入）
Irodori 模型支持将 Emoji 作为无声的情绪 Conditioning Token，**不会被读出发音，直接改变局部声调与语流**。
- **强制规范**：**禁止仅在整句句首单挂一个 Emoji**。必须跟随语义转折，在分句或意群前按需嵌入；重复同一 Emoji 可强化语气（如 `😭😭`）。
- 示例：
```powershell
# 句中多情绪转折
& $vc speak --voice mado-kano-lora \
  --text "最初は😊楽しく笑ってたのに、急に雷が鳴って😲ビックリして…[pause:300]最後は😭😭怖くなっちゃいました…" \
  --display "起初明明还开心地笑着，突然打起雷来吓了一跳……最后害怕得哭出来了……"

# 配合 pause 的反差语调
& $vc speak --voice sora-ikaros-lora \
  --text "マスター😏少しだけ耳を貸してください。[pause:300]👂ここだけの秘密です…[pause:300]🫶大好きです。" \
  --display "Master，请凑近听一下。这是我们两人的秘密……最喜欢您了。"
```

### 45 种标准支持的 Emoji 速查
| 类型 | Emoji 与效果 |
|---|---|
| **悲伤与抽泣** | `😭` 痛哭、呜咽 \| `🥺` 声线发颤、委屈不安 \| `😖` 痛苦、强忍 |
| **愤怒与爆发** | `😠` 生气、恼怒 \| `💥` 情绪爆发、厉声发难 \| `😒` 咋舌、冷淡厌弃 |
| **语速与节奏** | `⏩` 语速加快、连珠炮式吐字 \| `🐢` 放慢语速、一字一顿 \| `⏸️` 沉吟停顿 |
| **喜悦与欢快** | `😊` 开心愉悦 \| `😆` 灿烂大笑 \| `😏` 调侃戏谑、撒娇 \| `🫶` 温柔依恋 |
| **惊异与害羞** | `😲` 吃惊感叹 \| `😮` 倒吸凉气 \| `🫣` 害羞脸红、捂脸 |
| **声学与场景** | `👂` 耳边低语、ASMR 贴耳感 \| `😮‍💨` 叹气、吐息 \| `📞` 电话或广播音质 \| `🎵` 哼歌律动 |

---

## 3. 音色包查询与管理

```powershell
& $vc voices
```
输出形如：
```
sora-ikaros-lora     伊卡洛斯                     lora-adapter
mado-kano-lora       常磐华乃 (LoRA)              lora-adapter
ak-pepe-lora         佩佩 (LoRA)                  lora-adapter
```
- `--voice` 参数必须传第一列的唯一 `id`。
- 如果用户请求的音色未安装，应明确如实告知，严禁编造不存在的 id。

---

## 4. 故障自检与诊断

运行 `& $vc doctor` 检查状态：
```
runtime      reachable, api v1
token        accepted
engine       managed=true running=true model_loaded=true
voice packs  6
presenters   1
```

| 现象 / 报错码 | 根因与动作 |
|---|---|
| `GET /api/health` 连不上 | 服务未启动。提示用户启动 `VoiceCore.exe`，或由命令行拉起后台运行时。 |
| `presenters: 0` / 听不到声音 | 字幕进程未运行。启动 `<install-root>\bin\presenter\VoiceCorePresenter.exe` 或使用 `--play always`。 |
| `voice_pack_not_found` | 检查 `& $vc voices`，使用列表中存在的真实 id。 |
| `invalid_request` | 参数错误，检查是否漏传 `--text` 或 `[pause:N]` 数值超出 1–10000 范围。 |
| `resource_busy` (429) | GPU 正在被独占占用（如后台正在进行 LoRA 训练）。等训练结束或取消训练后再调用。 |
