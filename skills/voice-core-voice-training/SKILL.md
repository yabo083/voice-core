---
name: voice-core-voice-training
description: 在本机用 voice-core 的训练脚本做一个新的 LoRA 音色包并装进 runtime：语料校验、潜变量、训练、试听、相似度评分、安装六步，全程由你在 shell 里执行。当任务涉及训练新音色 / 音色克隆 / LoRA 微调 / 注册音色包（`voicepack.json`、`config.json` 的 `voicePacks`）/ 挑检查点与验收 / 训练跑到哪一步了或卡在哪里 / voice-core 的「训练」页显示的那些运行时使用。
---

# voice-core-voice-training：训练一个新音色包

六步流水线，一次 50–90 分钟的 GPU 作业，**由你在 shell 里执行**。面板的「训练」页不发起训练：
它只读你写下的两个状态文件（§3）做可观测性，所以它会显示它从未启动过的运行——这是正常情况。

## 0. 这台机器上的位置与前提

本机安装在 `E:\NewToolBox\voice-core\`：训练脚本在 `scripts\training\`（每个脚本 `--help` 说明自己的参数），
数据目录在 `data\`，引擎解释器在 `runtime\python\Scripts\python.exe`。

不在这台机器上：解释器看 `data\runtime.json` 的 `ttsPython`，再回落到安装根下的
`runtime\python\Scripts\python.exe`；`bin\voice-core-runtime.exe --print-layout` 不启动任何东西，
就把解释器、worker 脚本、引擎根、HF 缓存和数据目录逐项打成 `ok` / `MISSING`：

```
install root   E:\NewToolBox\voice-core
data dir       E:\NewToolBox\voice-core\data
packs          E:\NewToolBox\voice-core\data\config.json
interpreter    ok      ...\irodori-tts\env\Scripts\python.exe
worker script  ok      E:\NewToolBox\voice-core\runtime/worker/irodori/worker.py
engine root    ok      ...\irodori-tts
HF_HOME        ok      ...\irodori-tts\model\huggingface
```

`interpreter` 是 `MISSING` 就别开始：环境没部署好，交给用户在 GUI 的部署页做，
**不要自己去下 4.8 GB 模型**，也不要硬编码 `HF_HOME` 或权重目录。

runtime 的 HTTP 在 `http://127.0.0.1:8760`，鉴权是 `Authorization: Bearer <token>`，token 的值在
数据目录的 `token.txt`（本机 `E:\NewToolBox\voice-core\data\token.txt`）。整条流水线只需要它一个路由：
`POST /api/sleep`（§1 的第一条硬约束）。**不要把 token 写进代码、日志、提交、报告或任何会外发的内容。**
完整 HTTP 契约在 `E:\NewToolBox\voice-core\docs\api.md`。

## 1. 动手前先记住这几条

- 音色包**只用 LoRA**。参考音频（`reference-audio`）仍然能注册，适合"只要音色、不要风格"，
  一条片段就够、不用训练（§5）；Speaker Inversion 那条路已经停做。
- LoRA 要音频 + **与音频同语言**的逐条转写，约 60–70 条 / 总时长 ~15 分钟够用。中文转写配日语音频会让
  adapter 学到生成时不成立的文本域映射（踩过：当前引擎的文本编码器读日语）。
- 选 checkpoint 看 val loss，不要拿最后一步。**每个检查点的名字都带 val loss**，没有"不知道好坏"的目录：
  `checkpoint_best_val_loss_*` 只有一个（最低的那个），其余候选是 `checkpoint_val_loss_<步数>_<损失>`。
  权重逐字节相同的重复目录会在训练结束时合并掉，所以看到的每一个都是不同的权重。
- **验证频率、保存频率、早停都是按语料自动算的，不要手填。** 每 5–8 个 epoch 验证并保存一次（取还能留出
  足够验证次数的最宽间隔），连续 5 次验证没有新低就停止。实测：69 条每 40 步、163 条每 88 步、400 条每
  200 步。所以 `max_steps` 是预算上限、不是预期时长——**跑到一半就停是正常结果，不是失败**，摘要里会写
  停在第几步、因为什么停。要自己定就在末尾传 `-- --valid-every N`，整套推导随之关掉。
- 开训前把数据集那一步的质量报告读完。削波、响度、底噪、首尾静音、真实带宽都在里面，**这是花掉一小时之前
  唯一便宜的发现问题的机会**。削波常常是母带自带的、重新下载修不掉；要不要筛掉是用户的决定，默认一条不筛。
- 验收看相似度分布的下限，不看均值（本例均值 0.771 / p10 0.703）。§4 是这两条的具体读法。

两条硬约束，动手前先读：

1. **显卡是独占的。** 第一个用显卡的步骤（latents）之前让 runtime 放手，否则引擎和训练器抢同一块显存：

   ```powershell
   $token = (Get-Content 'E:\NewToolBox\voice-core\data\token.txt' -Raw).Trim()
   $auth  = @{ Authorization = "Bearer $token" }
   Invoke-RestMethod -Method Post -Uri 'http://127.0.0.1:8760/api/sleep' -Headers $auth
   ```

   runtime 没起时这条会报连不上，那就说明没人占着显卡，继续。**训练期间不要合成语音。**
2. **2000 步的 LoRA 是 50–90 分钟**（batch 16，RTX 5060 Ti，1.9–2.3 s/step）。不要在前台干等，
   也不要中途改配置——配置只在下一次运行生效。

## 2. 六步流水线

需要用户给两样东西：**音频目录**（一条切片一个文件，48 kHz / 单声道 / 16-bit PCM WAV 最好，单条
1–30 秒）和**音色包 id**（只允许英文字母、数字、`.`、`-`、`_`）。逐条转写可以是与音频同名的
`<切片名>.txt`，也可以是一个 csv / tsv / jsonl 映射；不在音频目录里就加 `--transcripts <文件或目录>`。

```powershell
$py = 'E:\NewToolBox\voice-core\runtime\python\Scripts\python.exe'
$T  = 'E:\NewToolBox\voice-core\scripts\training'
$ID = 'my-voice'
$A  = '<音频目录>'
# 暂存目录：面板按这个路径清点产物
$D  = "E:\NewToolBox\voice-core\data\cache\train\$ID"
# 状态文件：面板按这个文件名发现这次运行
$S  = "E:\NewToolBox\voice-core\data\logs\training-$ID.status.json"

& $py "$T\irodori\prepare_dataset.py" --json --status-file $S `
    --recursive --audio-dir $A --speaker-id $ID --out-dataset "$D\dataset.jsonl"

& $py "$T\irodori\encode_latents.py" --json --status-file $S `
    --dataset-file "$D\dataset.jsonl" --latent-dir "$D\latents" `
    --out-manifest "$D\train_manifest.jsonl"

& $py "$T\irodori\run_training.py" --json --status-file $S `
    --config lora --manifest "$D\train_manifest.jsonl" --output-dir "$D\lora"

& $py "$T\irodori\generate_samples.py" --json --status-file $S `
    --lora "$D\lora" --no-ref --out-dir "$D\samples"

& $py "$T\irodori\evaluate_similarity.py" --json --status-file $S `
    --label $ID --ref-dir $A --tests "$D\samples\*.wav" --out-dir "$D\score"

& $py "$T\install_pack.py" --json --status-file $S `
    --pack "$D\lora\<选中的检查点>" --id $ID --data-dir 'E:\NewToolBox\voice-core\data' --force
```

- `--config lora` 取的是 `$T\irodori\lora.yaml`（batch 16 / 2000 步 / lr 1e-4 / 每 500 步存点、
  另外留 3 个 val loss 最好的）。**不要改那个模板**，它的注释是每个值的唯一说明；要改参数就在命令末尾
  加 `--` 再透传给上游训练器，例如 `-- --max-steps 1000 --batch-size 8`。改步数是安全的：warmup 与
  stable 会按模板的 5% / 75% 比例跟着缩放，学习率衰减照常发生，流水线会把推导出的三个数打出来。
- 三个质量筛选开关默认全关，要用就加在 `prepare_dataset.py` 后面：`--drop-clipped`、`--min-snr <dB>`、
  `--min-bandwidth <Hz>`。**先看不带开关的报告，再决定值**——某个语料 163 条里 77 条削波，无脑开等于砍掉一半。
- **第 6 步（安装）是用户的决定，不要自己挑检查点**——面板的「训练成果」表就是为这个决定存在的。
  给出候选（§4）、等用户点名再执行。包的显示名默认等于 id，要别的名字就带 `--name "显示名"`
  （还可带 `--character`、`--avatar`），它们写进包自己的 `voicepack.json`（§5）。
  `install_pack.py --dry-run` 会打印它要拷什么、要往 `config.json` 插哪一条 JSON，什么都不改。
- `--status-file` 指向的目录和文件名**不要改**：`data\logs\training-<音色包 id>.status.json` 是面板
  发现一次运行的唯一依据，写到别处这次运行就没人看得见。
- 暂存目录 `data\cache\train\<音色包 id>\`（数据集、QA 报告、潜变量、检查点、试听、评分）可以整个删掉重跑；
  §3 那两个状态文件不在里面，删了暂存也还在。

## 3. `--json` 协议与两个文件

`--json` 之下每一步在 stdout 上逐行吐 JSON，人类可读输出全部转到 stderr。七个键固定存在：
`ts stage event message done total remedy`（train / score 两步另带 `checkpoint`），
`event` ∈ `start` / `progress` / `log` / `ok` / `skip` / `fail`。
**一步失败时它吐 `event:"fail"` 并带 `remedy`，然后退出码 0**；非 0 退出码只意味着 argv 写错了。
所以**判断成败要读事件流或状态文件，不要读退出码**。
`--status-file` 把同一份事件流折成两个文件，都在 `data\logs`，都不需要 GUI 开着，也不需要订阅任何事件：

| 文件 | 回答 |
|---|---|
| `training-<音色包 id>.status.json` | 现在怎么样。一次读取回答"在跑吗、哪一步、多远、哪里失败" |
| `training-<音色包 id>.jsonl` | 发生了什么。每行一个事件，键就是上面那七个 |

```powershell
# 要轮询的就是这个，几秒一次够了
Get-Content -Raw 'E:\NewToolBox\voice-core\data\logs\training-my-voice.status.json' |
  ConvertFrom-Json | Select-Object live,pid,stage,state,message,done,total,failed_stage,failure,remedy,updated
# 跟着看
Get-Content -Wait -Tail 20 'E:\NewToolBox\voice-core\data\logs\training-my-voice.jsonl'
```

`status.json` 的字段：`live` 在跑没跑；`pid` 是**正在写这个文件的那个步骤进程**；`stage` 是流里最后提到
的一步（`dataset` / `latents` / `train` / `samples` / `score` / `install`），`state` 是那一步的状态；
`done` / `total` 是那一步的进度；`failed_stage` / `failure` / `remedy` 是失败那一步、失败原因和该怎么办；
`stages` 数组里六步各一行，跑完的那几步保留自己的 `message` 和起止时间。

两个坑。**`live` 只在 `pid` 那个进程还活着时才算真的**：被硬杀掉的步骤没机会把 `live` 改回 `false`，所以
`live: true` 而 `updated` 是十分钟前，就是已经死了——文件比进程活得久，这正是它存在的理由（面板读这两个
文件时会自己按 `pid` 判活，你按同一条规则判就和面板一致）。**`interrupted` 不是 `fail`**：`state` 除了
六个事件名还有 `pending`（还没开始）和 `interrupted`（跑着被中断的）；`fail` 是那一步自己解释过的，
`remedy` 里有办法，`interrupted` 没有解释也没有 `remedy`。

## 4. 选检查点与验收

`$D\lora` 里**每个目录名都带 val loss**，名字可以直接信：`checkpoint_best_val_loss_*` 只有一个，就是最低的；
其余是 `checkpoint_val_loss_<步数>_<损失>`，都经过验证只是不是最低。每 100 步验证一次（2000 步共 20 次），
其中最好的 5 个留下来当候选。权重完全相同的重复目录（周期性存点 / `checkpoint_final` 常常和某个候选一样）
会被合并，保留信息量最大的那个名字。候选打平时依次比：val loss 更低、步数更早。
第 4 步会给每个检查点各生成一组固定种子的试听样本（组名 `lora_<检查点目录名>`），第 5 步把它们和参考语料
比出相似度，写到 `$D\score\<音色包 id>.json`：

- `groups` 数组已经按 `lower_bound` 从高到低排好，`lower_bound` 是**那一组里最差的一条**——听众最终听到的
  就是它，所以选它、不看 `mean`。
- `ceiling_loo` 是语料自身的留一法上界（同一个人的自然波动），生成的片段不该越过它。
  `below_natural_p10: true` 的组说明下限已经掉到自然波动的 p10 以下，装之前先让用户听一遍。
- 步数多不等于更好：本项目参考运行里 1000 步优于 2000 步，2000 步已经过拟合。

向用户报候选时给三样：检查点目录名、它名字里的 val loss、它的 `lower_bound`。挑哪个是用户的决定。
装完 `voices` 立刻能列出来（runtime 按 mtime 重载 `config.json` 那一段，不用重启）；
之后怎么调用、怎么试听那个包，用 `voice-core-tts` 技能。

## 5. 包是怎么注册的：三种 kind、两个文件

第 6 步的 `install_pack.py` 就是替你写下面这些东西。手工注册一个 `reference-audio` 包（只要音色、
不要风格，不用训练）也是同一套规则，下面就是完整规则，不需要别的文档。

三种 `kind`：

| `kind` | 产物 | `path` 指向 |
|---|---|---|
| `lora-adapter` | **目录**，含 `adapter_config.json` + `adapter_model.safetensors` | 该目录 |
| `speaker-embedding` | **单文件，文件名必须以 `.speaker.safetensors` 结尾** | 该文件 |
| `reference-audio` | 参考音频 | 音频文件 |

改名踩过的坑：把 SE 文件改成没有后缀的名字会被引擎按名字拒绝
（`Speaker Inversion embeddings must use the '.speaker.safetensors' suffix`），拷进 `data\voicepacks\` 时保留原文件名。

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

## 6. 不要做的事

- **不要替用户挑检查点，也不要跳过第 5 步的评分。** 装哪一个是用户的决定，你给候选和数字。
- **不要在训练期间合成语音**，也不要在跑之前忘了 `POST /api/sleep`：显卡是独占的，两边一起用就都慢，
  或者直接 OOM。
- **不要改 `lora.yaml`**，也不要中途改配置：改了只在下一次运行生效，本次白跑。
- **不要改 `--status-file` 的目录或文件名**，不要把暂存目录挪到别处：面板和其他 agent 就是按
  `data\logs\training-<id>.status.json` 和 `data\cache\train\<id>\` 找这次运行的。
- **不要按退出码判断成败**（§3），也不要把 `interrupted` 当 `fail` 报给用户。
- **不要手改 `data\config.json`**：装包用 `install_pack.py`，它只**追加** `voicePacks` 条目；
  真要手写也只追加、先征得用户同意，文件里的注释和其他段落一个字都不要动。
  `data\runtime.json`（引擎与模型路径）是部署产物，改了不重启 runtime 也不会读，重启会打断用户。
- **不要假设模型路径，也不要自己去下模型。** 解释器、引擎根、HF 缓存每台机器都不一样，
  来自 `data\runtime.json`；要知道就跑 `--print-layout`（§0）。
- **不要把 token 写进任何会外发的内容**，包括你的回复、日志和提交。
- 用中文转写配日语音频、拿最后一步的检查点、按均值验收——这三件都踩过，都在 §1。
