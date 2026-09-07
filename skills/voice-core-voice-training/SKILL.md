---
name: voice-core-voice-training
description: 用 voice-core 脚本训练 LoRA 音色包并注册入库。涵盖语料质检、潜变量编码、LoRA微调、试听生成、说话人相似度评分与安装注册全流程。
---

# voice-core-voice-training：音色训练与注册

音色训练是一套 6 步完整的 GPU 流水线作业，**由 Agent 在终端环境中执行**。前端控制台的「训练 Tab」作为只读看板，通过监听状态文件呈现进度。

---

## 0. 核心前置与硬件感知（必须执行）

### 1. 动态显存感知与超参数自适应（严禁盲从固定显存门槛）
开训前必须使用 `nvidia-smi` 获取当前显卡可用显存，并动态计算 `batch_size` 与梯度累积（等效 Batch 为 16）：
- **16 GiB 显存**：使用默认配置 `-- --batch-size 16 --gradient-accumulation-steps 1`
- **8–12 GiB 显存**：动态下调 `-- --batch-size 8 --gradient-accumulation-steps 2`（显存直降约 45%）
- **6 GiB 显存**：动态下调 `-- --batch-size 4 --gradient-accumulation-steps 4`

### 2. 释放推理引擎显存（独占 GPU）
在进入第二步潜变量编码前，必须让常驻的推理引擎释放显存，避免显存竞争或 OOM：
```powershell
$token = (Get-Content '<install-root>\data\token.txt' -Raw).Trim()
Invoke-RestMethod -Method Post -Uri 'http://127.0.0.1:8760/api/sleep' -Headers @{ Authorization = "Bearer $token" }
```

---

## 1. 语料规模与少样本训练规范

Irodori-TTS v4.1-Small 架构原生针对少样本克隆设计（上游模型卡及 `parameters.md` 官方建议）：
- **设计门槛**：官方参考基准起点仅约 30 秒至 2 分钟干净语音（多条短切片随机拼接训练）。
- **语料时长分级预期**：
  - **30 秒 ~ 2 分钟（约 20–50 条短切片）**：足以支撑 LoRA 学习到独特的音色辨识度，可快速出成果；
  - **15 分钟以上（约 60–100 条切片）**：进一步稳固声线与泛化稳定性。
- **严禁门槛焦虑**：切勿因语料仅有几分钟而拒绝开训；只要具备几十秒至数分钟干净音频即可起跑，训练过程中通过 Val Loss 监控过拟合即可。

---

## 2. 六步训练流水线（标准执行命令）

安装根目录记为 `<install-root>`。
准备条件：**纯净音频目录**（单声道/48kHz/16-bit WAV，1–30秒切片）和**音色包 ID**（仅字母、数字、破折号）。
$root = '<install-root>'
$py   = "$root\runtime\python\Scripts\python.exe"
$T    = "$root\scripts\training"
$ID   = 'my-voice'
$A    = '<音频目录绝对路径>'
$D    = "$root\data\cache\train\$ID"
$S    = "$root\data\logs\training-$ID.status.json"

# 步骤 1：语料整理与音频质量校验
& $py "$T\irodori\prepare_dataset.py" --json --status-file $S `
    --recursive --audio-dir $A --speaker-id $ID --out-dataset "$D\dataset.jsonl"

# 步骤 2：潜变量特征编码（Latents）
& $py "$T\irodori\encode_latents.py" --json --status-file $S `
    --dataset-file "$D\dataset.jsonl" --latent-dir "$D\latents" `
    --out-manifest "$D\train_manifest.jsonl"

# 步骤 3：启动 LoRA 微调（透传自适应 batch 参数）
& $py "$T\irodori\run_training.py" --json --status-file $S `
    --config lora --manifest "$D\train_manifest.jsonl" --output-dir "$D\lora" `
    -- --batch-size <自适应值> --gradient-accumulation-steps <自适应值>

# 步骤 4：各候选检查点试听音频批量生成
& $py "$T\irodori\generate_samples.py" --json --status-file $S `
    --lora "$D\lora" --no-ref --out-dir "$D\samples"

# 步骤 5：说话人相似度自动化打分评估
& $py "$T\irodori\evaluate_similarity.py" --json --status-file $S `
    --label $ID --ref-dir $A --tests "$D\samples\*.wav" --out-dir "$D\score"

# 步骤 6：安装注册（待用户确认后执行）
& $py "$T\install_pack.py" --json --status-file $S `
    --pack "$D\lora\<选中的检查点目录名>" --id $ID --name "<显示名称>" --character "<角色名>" `
    --data-dir "$root\data" --force
```

## 3. 检查点挑选与用户验收原则

1. **依据验证损失（Val Loss）与相似度下限（lower_bound）挑选**：
   - 检查点命名形如 `checkpoint_val_loss_<步数>_<损失>`，损失越低通常泛化越好。
   - 打开 `$D\score\$ID.json` 查看评分，优先选取 `lower_bound`（最差单句相似度下限）最高的前 2–3 个检查点。
2. **严禁 Agent 自作主张直接安装**：
   - 完成步骤 5 后，Agent 必须向用户汇报候选列表（含步数、Val Loss、相似度指标），**由用户选定后，再执行步骤 6 安装**。

---

## 4. 注册生效与配置文件规范

- `install_pack.py` 会将权重复制到 `data/voicepacks/<ID>/`，并在其中生成 `voicepack.json`，同时向 `data/config.json` 的 `voicePacks` 数组中追加该包的相对路径。
- `config.json` 具备 mtime 监听，注册完成后无需重启应用，`voice-core voices` 会即刻展示新音色包。

---

## 5. 严禁行为清单（红线）

- ❌ **严禁机械复读“必须 16GB 显存”或“必须几十分钟长语料”**：遇到中小显存卡主动自适应参数；几十秒至几分钟少样本语料完全符合官方设计。
- ❌ **严禁训练过程中发起语音合成**：显卡必须由训练器独占。
- ❌ **严禁替用户跳过试听评分与挑点决策**：最终安装哪一个 checkpoint 必须征求用户同意。
- ❌ **严禁篡改 `--status-file` 路径**：必须写入 `data/logs/training-<ID>.status.json`，否则 GUI 无法联动监控。
