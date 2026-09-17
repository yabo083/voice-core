# 角色卡（Character Card / Persona）Roadmap

> 状态：**未开始（2026-09-09 讨论定稿）**。目标：把"酒馆式角色卡"塞进音色包，让任何 agent
> harness 在开会话时自动加载当前角色的人设并持久化，配合已有的 voice-core 语音能力获得
> galgame 式的实时陪玩体验。当前动因：与角色的实时交流全靠会话内口头扮演——人设随会话消失，
> 切音色即断裂，共同经历（游戏截图闲聊、偏好）无法跨会话携带。

---

## 0. 问题拆解

"角色卡"在 agent harness 场景是两层东西：

| 层 | 内容 | 生命周期 |
|---|---|---|
| 静态人设卡 | 身份、性格、口癖、开场白、示例对话、世界观（lore） | 跟音色包走，基本不变 |
| 动态记忆 | 关系进度、共同经历、偏好 | 每次对话增长 |

两个痛点各有归属：人设断裂是静态层缺失；经历丢失是动态层缺失（MVP 不做，见 §6）。

## 1. 卡片放哪（已定）

```
voicepacks/<pack-id>/
  voicepack.json      ← 语音契约，不动。可选加一行引用："persona": "card.md"
  card.md             ← 人设卡（约定文件名，emitter 直接找它）
  lore/*.md           ← 大段世界观/设定，按需读，不注入
```

**不塞进 voicepack.json**。理由：voicepack.json 是 runtime 与面板（`config_edit.rs`）都会重写的
文件，大段人设有被重写丢失的风险；且 `/api/voices` 按 mtime 热重载，不该拖着人设跑。卡片是
给 harness 消费的，不是给 TTS runtime 消费的——两个关注点分开，整目录打包分发不受影响。

## 2. 卡片格式（已定：单文件 Markdown + YAML frontmatter）

正文即注入内容，渲染器几乎零转换；段落命名对齐 SillyTavern 字段（`name/description/
personality/scenario/first_mes/mes_example` ↔ 下面的段落），将来做 ST 导入就是一张字段映射表，
现在不做导入。

```markdown
---
id: mado-kano
name: 常磐华乃
voice: mado-kano-lora        # speak --voice 的绑定；切换时人设与声线一条命令一起换
emotions:                    # 该角色偏好的 conditioning emoji（可选）
  default: ""
  shy: 🫣
  angry: 😠
  whisper: 👂
lore:                        # 触发表：提到什么 → 读哪个文件（穷人版 lorebook）
  章鱼烧: lore/takoyaki.md
---

## 开场白
（首次入戏的第一句话）

## 身份与性格
## 说话风格
## 示例对话
## 行为规则
（说话前读 skill://voice-core-tts；按 emotions 选 emoji；不出戏）
```

voice-core 特有的加分项：`emotions` 映射把 45 个 Irodori conditioning emoji 变成
**声音表演指导**——这是酒馆卡没有的能力。注入体量目标 500–800 token。

## 3. 注入机制（已定：project 目录级 sentinel 块）

`persona use <id>` 默认以 cwd 为目标目录，把卡片渲染成紧凑文本写入各 harness 的**项目级**
context file，统一用 sentinel 块包裹、幂等重写、保留用户手写内容：

```
<!-- voice-core:persona begin (id=xxx) -->
...
<!-- voice-core:persona end -->
```

| harness | 写入文件 | 备注 |
|---|---|---|
| omp | `<dir>/.omp/AGENTS.md` | native provider，优先级 100 |
| Claude Code | `<dir>/.claude/CLAUDE.md` | |
| Codex + OpenCode | `<dir>/AGENTS.md` | 两家读同一个 standalone 文件，合写一份 |
| Gemini CLI | `<dir>/.gemini/GEMINI.md` | |

默认四份全写（目标就是"任何 harness"），`--harness omp,claude` 可裁剪，`--dir` 可换目录。

**为什么选 project 而不是 user 级**（2026-09-09 决策）：
- omp 的 user 级 native 文件 `~/.omp/agent/AGENTS.md` 优先级最高且**唯一存活**，创建它会整个
  遮蔽现有 `~/.agents/AGENTS.md`（agents provider），这是必须绕开的坑。
- user 级注入会让纯干活的会话也带着人设卡。
- 本机 `~/.agents/AGENTS.md` 与 `~/.config/opencode/AGENTS.md` 是硬链接对——任何"temp 文件 +
  rename"写法都会替换 inode、静默拆掉链接对。选 project 级后这个坑完全无关。

使用约定：`C:\tmp` 是万能会话目录，在那里 `use` 等于全局生效；想隔离就给每个游戏建目录
（如 `C:\tmp\gal-mado`）再 `use`。注意 omp 里 `<dir>/.omp/AGENTS.md` 会遮蔽同目录的
standalone `AGENTS.md`，纯干活目录不要 `use`。

## 4. 注入内容分层

- **注入**：身份、性格与说话风格、开场白第一条、lore 触发表、行为规则
  （"先读 skill://voice-core-tts 再说话"）、emotions 映射。
- **不注入**：示例对话全文、lore 正文。靠触发表让模型按需 `read`。
  没有 ST 那种服务端关键词注入引擎，靠模型自觉 + 卡内祈使句强化；对 token 富余的本地
  harness 够用，代价是偶尔忘触发。

## 5. CLI 面

```
voice-core persona use <pack-id> [--dir .] [--harness all|omp,claude,codex,gemini]
voice-core persona unuse [--dir .]
voice-core persona show
```

配 `voice-core-persona` skill（或并入现有 tts skill）教 agent：用户说"换成美游"→
`persona use ba-miyu-lora` → 立刻用美游声线说她的开场白。当次会话 agent 直接演（即刻生效），
下个会话靠已写入的 context file 自动入戏。

## 6. 动态记忆（MVP 不做，2026-09-09 决策）

跨会话连续性暂靠 harness 自带能力（omp 的 resume/会话历史）。将来做时：
`<pack>/memory/journal.jsonl` append-only（`{ts, event}` 每行自包含），卡内指令驱动 agent
自己写，渲染时取最近 N 条 + 压缩摘要。对 card.md 零破坏——渲染器多读一个文件即可。
局限：无文件能力的 harness（远程 ChatLuna/QQ bot）用不了，那是更后面的 HTTP 投递问题。

## 7. 实施清单（做的时候照这个走）

1. card.md schema 定稿 + 渲染器（frontmatter 解析、lore 表、emotions、段落抽取）。
2. 四个 harness emitter（sentinel 块读写、幂等、保留用户内容）+ `persona use/unuse/show`。
3. `voice-core-persona` skill。
4. 给 mado-kano 写第一张实测卡，`C:\tmp` 下建隔离目录端到端验证：
   omp / Claude Code / Codex / Gemini 各开一个新会话确认自动入戏 + 语音开口。
5. `docs/` 补一页 persona 规范（对齐 voicepack-spec.md 的写法）。

估：一个会话可全部落地（CLI + 四 emitter + skill + 一张实测卡）。

## 8. 阶段 2（想到但没排期）

- SillyTavern v2/v3 卡导入转换器（格式兼容思路见 §2）。
- manager 面板的卡片编辑 UI。
- lore 触发调优、记忆压缩。
- `GET /api/voices` 透出 persona 路径，供远程 harness（ChatLuna 等）发现与投递。
