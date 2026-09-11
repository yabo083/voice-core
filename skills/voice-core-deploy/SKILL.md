---
name: voice-core-deploy
description: 部署或修复 voice-core 本地 TTS 运行环境：定位安装、检测引擎/模型/显存缺口、执行七阶段 bootstrap、重启面板并端到端冒烟。首次安装后配置、升级后环境损坏、换机迁移时使用。
---

# voice-core-deploy：环境部署与修复

**先读这节的两个事实，再动手：**

1. **产品是免重启部署的**：安装器只复制程序与 skill（148 MB 级）。能出声的前提是
   `scripts\bootstrap.ps1` 七阶段跑完（engine / codec / venv / models 共 4.44 GiB
   下载，本机已有可复用资产时跳过）。安装完没有声音 = bootstrap 没跑，不是坏了。
2. **只有一个入口程序**：`<install-root>\VoiceCore.exe`。它自己管理 runtime 与字幕
   进程。**永远不要手动启动** `bin\voice-core-runtime.exe` 或 presenter。

---

## 0. 定位安装根（第一步，禁止跳过）

安装目录由用户在安装器中选择，**没有固定盘符**。按顺序探测：

```powershell
# 1) 注册表（安装器写入，AppId 固定不变；唯一可靠来源）
$inst = (Get-ItemProperty "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\{29FD851D-5FAD-563F-ADB6-2AC7B34E76D1}_is1" -ErrorAction SilentlyContinue).InstallLocation
# 2) 兜底：探测常见盘符 <盘>:\NewToolBox\voice-core
if (-not $inst) { $inst = @('C:','D:','E:') | ForEach-Object { "$_\NewToolBox\voice-core" } | Where-Object { Test-Path "$_\bin\voice-core.exe" } | Select-Object -First 1 }
# 3) 都没有 → 让用户指认安装目录（含 VoiceCore.exe 的目录），不要全盘搜索
```

后续所有命令以 `$root = '<探测到的路径>'` 为基准。

---

## 1. 诊断：分清「没装」还是「没跑」

```powershell
$root = '<第 0 步结果>'
& "$root\bin\voice-core.exe" doctor
```

按输出分流（先读 §1 再执行任何命令）：

| doctor 输出 | 判断 | 去哪 |
|---|---|---|
| `runtime reachable` + `presenters 1` + `voice packs ≥1` | 环境完好 | 直接能用，走 [[voice-core-tts]] |
| `runtime reachable` + `voice packs 0` | bootstrap 未跑或未完成 | §2 |
| `runtime NOT reachable` | 服务没起（可能开机未启面板） | §3 启动面板后重测 |
| `token rejected` / 程序不存在 | 安装损坏或被移动 | §4 修复 |

补充诊断（只读、不改任何东西）：

```powershell
& "$root\scripts\bootstrap.ps1" -CheckOnly        # 七阶段逐项：缺什么、怎么补、每项的 remedy
```

---

## 2. 执行部署（bootstrap 七阶段）

```powershell
# 全量：缺什么补什么，本机已有的资产（引擎树/venv/模型）自动复用不重复下载
pwsh -NoProfile -ExecutionPolicy Bypass -File "$root\scripts\bootstrap.ps1" -Json
```

- `-Json` 逐行输出事件（GUI 与 agent 解析用），不要去掉：人读时才省略。
- **可重复执行**：任何阶段中断（网络断、Ctrl+C）后直接重跑整条命令即可，完成的
  阶段报 `skip`。模型下载慢时可用 `-Only models` 单独重试该阶段。
- 阶段顺序：`preflight → engine → codec → venv → models → layout → smoke`。
  `smoke` 阶段会真实合成一句验证端到端，GPU 空闲时约 1–2 分钟。

有本机资产要复用时（显式传入会**替代**自动探测，不要传错目录）：

```powershell
& "$root\scripts\bootstrap.ps1" -EngineRoot 'D:\irodori-tts' -HfHome 'D:\hf' -VoicePacks 'D:\packs'
```

## 3. 启动面板与就绪确认

```powershell
Start-Process "$root\VoiceCore.exe"
# 就绪轮询：面板拉起 runtime 约需 10 秒，60 秒上限
$vc = "$root\bin\voice-core.exe"
foreach ($i in 1..12) { Start-Sleep 5; $d = (& $vc doctor 2>$null) -join ' '; if ($d -match 'reachable') { break } }
& $vc doctor   # 期望 runtime reachable, api v1 / presenters 1 / voice packs N
```

## 4. 安装损坏的修复

- **全局 skill 丢失**（`%USERPROFILE%\.agents\skills\voice-core-*`）：重跑安装器或
  手动复制 `$root\skills\voice-core-*\SKILL.md` 到该目录。
- **目录被移动/换机**：安装树是可移植的（内部相对路径），整体移动后重跑 §0 探测
  + §3；`data\runtime.json` 指向的外部引擎路径失效时重跑 §2。
- **程序文件缺失**：重新运行安装器（GitHub Release 的 setup.exe）覆盖安装。升级
  保留 `data\*`（配置/音色包/token）与 `runtime\python\*`、`models\*`（下载产物）。

## 5. 验证部署成功（收尾必做）

```powershell
& "$root\bin\voice-core.exe" voices            # 取第一行第一列的包 id（如 sora-ikaros-lora）
& "$root\bin\voice-core.exe" speak --voice <包id> --text 'デプロイ完了です。' --display '部署完成。' --wait
```


一句合成成功 + 字幕弹出 = 全链路（runtime / 引擎 / 显卡 / 音色包 / presenter）就绪。

**Agent 环境注意**：bash/MSYS shell 无法直接执行 .exe（报 command not found），一律
经 `pwsh -NoProfile -Command "& 'C:\...\voice-core.exe' ..."` 调用；复杂脚本先写成
`.ps1` 文件再 `-File` 执行。冷启动 15–50 秒，部署完的第一句建议提前告知用户。

## 故障速查

| 现象 | 原因与处理 |
|---|---|
| bootstrap `models` 阶段超时/中断 | 网络问题。重跑即可续传；持续失败时 `git config --global http.proxy http://127.0.0.1:7890` 后 `-Only models` |
| `smoke` 阶段失败 `resource_busy` | GPU 被占（训练/其他推理）。结束后重跑 |
| `-EngineRoot` 传入后报找不到 `webui\Irodori-TTS` | 该参数要的是**包含** `webui\Irodori-TTS` 的目录，不是引擎目录本身 |
| 安装器装完桌面无图标 | 检查开始菜单「voice-core」；首次运行 bootstrap 前面板部署屏会显示缺口清单 |
| 端口 8760 被占 | 找到并结束残留的 `voice-core-runtime.exe`，再启动面板 |
