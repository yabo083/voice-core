// The voices list screen, in both languages. `zh` is the source of truth; the `en`
// object types itself against it, so a key added above and forgotten below is a
// compile error. Widget vocabulary shared across screens (清除, 未设置, 移除 …) lives
// in `common.ts` and is referenced as `t.common.*` at the call sites.

export const voicesZh = {
  title: "音色",

  // --- the pack list --------------------------------------------------------------------
  listTitle: "已安装音色包",
  refresh: "刷新",
  addFolder: "添加目录…",
  addFile: "添加文件…",
  pickFolderDialog: "选择音色包目录 (LoRA)",
  pickFileDialog: "选择说话人嵌入文件 (*.speaker.safetensors) 或参考音频",
  configure: "配置",

  // --- registering a pack (the draft form) ----------------------------------------------
  draftTitle: "注册新音色包",
  register: "注册",
  cancel: "取消",
  characterPlaceholder: "留空使用名称",

  idLabel: "音色包 ID",
  idHint: "voicePackId，作为 POST /api/speak 接口的唯一调用标识。",
  nameLabel: "显示名称",
  characterLabel: "角色名称",
  characterHint: "字幕窗口展示的说话人名称。",
  avatarLabel: "头像图标",
  avatarHint: "字幕窗口显示的头像图片；将自动归档至音色包目录。",
  avatarPick: "选择图像…",
  avatarChange: "更改图像…",
  avatarPickDialog: "选择头像文件",
  avatarClear: "清除头像",
  langLabel: "支持语言",
  langHint: "以逗号分隔的语言代码（如 ja 或 ja,zh）。",
  engineLabel: "推理引擎",
  kindLabel: "模型类型",
  kindAutoHint: "已基于路径特征自动推断，可手动修正。",

  // per-kind descriptions, keyed to PackKind
  hintLoraAdapter: "LoRA 适配器目录，包含训练生成的权重文件及元数据。",
  hintSpeakerEmbedding: "独立说话人嵌入文件 (*.speaker.safetensors)。",
  hintReferenceAudio: "参考音频切片，推理引擎据此提取目标音色特征。",

  // draft validation
  validationTitle: "输入校验未通过",
  errBadId: "音色包 ID 仅支持小写字母、数字、点号、下划线及连字符 (-)，且不能以特殊符号开头。",
  errDuplicateId: (id: string) => `已存在相同 ID 的音色包：${id}。`,
  errNameEmpty: "音色名称不能为空。",
  errNoLanguage: "请至少指定一种语言代码（如 ja）。",
  errEngineEmpty: "推理引擎不能为空；针对 Irodori 架构请填写 irodori。",
  writeFailed: "写入配置文件 (config.json) 失败",

  // toasts
  registeredToast: (id: string) => `已成功注册音色包 ${id}`,
  removedToast: (id: string) => `已移除音色包 ${id}`,
  removeFailedToast: (detail: string) => `移除音色包失败：${detail}`,

  // --- a pack row -----------------------------------------------------------------------
  confirmRemove: "确认移除",
  removeTitle: "从 config.json 移除配置项",
  metaName: (name: string) => `名称 ${name}`,
  metaEngine: (engine: string) => `引擎 ${engine}`,
  metaLangs: (langs: string) => `语言 ${langs}`,
  unspecified: "未指定",

  // --- the empty state ------------------------------------------------------------------
  emptyTitle: "暂未安装音色包",
  emptyBody: "支持导入 LoRA 权重目录、独立 *.speaker.safetensors 嵌入文件或参考音频文件。",
  goDeploy: "前往部署语音引擎",
};

export const voicesEn: typeof voicesZh = {
  title: "Voices",

  listTitle: "Installed voice packs",
  refresh: "Refresh",
  addFolder: "Add folder…",
  addFile: "Add file…",
  pickFolderDialog: "Choose a voice-pack folder (LoRA)",
  pickFileDialog: "Choose a speaker-embedding file (*.speaker.safetensors) or reference audio",
  configure: "Configure",

  draftTitle: "Register a new voice pack",
  register: "Register",
  cancel: "Cancel",
  characterPlaceholder: "Leave blank to use the name",

  idLabel: "Voice pack ID",
  idHint: "voicePackId - the unique handle callers use against POST /api/speak.",
  nameLabel: "Display name",
  characterLabel: "Character name",
  characterHint: "Speaker name shown in the subtitle window.",
  avatarLabel: "Avatar",
  avatarHint: "Image shown in the subtitle window; archived into the pack folder automatically.",
  avatarPick: "Choose image…",
  avatarChange: "Change image…",
  avatarPickDialog: "Choose an avatar image",
  avatarClear: "Clear avatar",
  langLabel: "Languages",
  langHint: "Comma-separated language codes (e.g. ja or ja,zh).",
  engineLabel: "Inference engine",
  kindLabel: "Model type",
  kindAutoHint: "Inferred from the path; adjust if needed.",

  hintLoraAdapter: "A LoRA adapter folder holding the trained weights and their metadata.",
  hintSpeakerEmbedding: "A standalone speaker-embedding file (*.speaker.safetensors).",
  hintReferenceAudio: "Reference audio clips the engine uses to extract the target voice.",

  validationTitle: "Validation failed",
  errBadId:
    "Voice pack IDs accept lowercase letters, digits, dots, underscores and hyphens (-), and must not start with a symbol.",
  errDuplicateId: (id) => `A voice pack with this ID already exists: ${id}.`,
  errNameEmpty: "The display name cannot be empty.",
  errNoLanguage: "Specify at least one language code (e.g. ja).",
  errEngineEmpty: "The inference engine is required; use irodori for the Irodori architecture.",
  writeFailed: "Failed to write config.json",

  registeredToast: (id) => `Voice pack ${id} registered`,
  removedToast: (id) => `Voice pack ${id} removed`,
  removeFailedToast: (detail) => `Failed to remove the voice pack: ${detail}`,

  confirmRemove: "Remove",
  removeTitle: "Remove the entry from config.json",
  metaName: (name) => `Name ${name}`,
  metaEngine: (engine) => `Engine ${engine}`,
  metaLangs: (langs) => `Languages ${langs}`,
  unspecified: "unspecified",

  emptyTitle: "No voice packs installed",
  emptyBody: "Import a LoRA weight folder, a standalone *.speaker.safetensors embedding, or a reference-audio file.",
  goDeploy: "Deploy the speech engine",
};
