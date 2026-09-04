// Voices: the voice pack registry, i.e. a view onto config.json's voicePacks.
//
// list_voices() answers whether the runtime is up or not (down = read from
// config.json directly), so this screen never has a "start the service first"
// state. Adding a pack is a two-step flow on purpose: pick, then confirm the
// metadata, because id / character / languages are what a caller will address the
// voice by and guessing them silently produces a pack nobody can use.

import { el, fill } from "../dom";
import { baseName, slugify } from "../format";
import { icon } from "../icons";
import {
  ipcMessage,
  importAvatar,
  pickFile,
  pickFolder,
  registerPack,
  removePack,
  type Pack,
  type PackKind,
} from "../ipc";
import { refreshInventory, refreshVoices, voices } from "../state";
import { toast } from "../toast";
import { button, chip, emptyState, field, navigate, note, panel, openButton, pathText } from "../ui";

const KIND_LABEL: Record<PackKind, string> = {
  "lora-adapter": "LoRA 适配器",
  "speaker-embedding": "说话人嵌入",
  "reference-audio": "参考音频",
};

const KIND_HINT: Record<PackKind, string> = {
  "lora-adapter": "一个目录，里面是训练出来的 LoRA 权重。",
  "speaker-embedding": "单个 *.speaker.safetensors 文件。",
  "reference-audio": "一段参考音频，引擎按它的音色说话。",
};

/** A directory is a LoRA adapter; a lone file is an embedding unless it is audio.
 *  This is the same rule scripts/training/install_pack.py applies when it lays a
 *  trained pack down on disk. */
function inferKind(path: string, isFolder: boolean): PackKind {
  if (isFolder) return "lora-adapter";
  const lower = path.toLowerCase();
  if (/\.(wav|flac|mp3|ogg|m4a)$/.test(lower)) return "reference-audio";
  return "speaker-embedding";
}

interface Draft {
  path: string;
  kind: PackKind;
  id: string;
  name: string;
  character: string;
  languages: string;
  engine: string;
  /** Already copied into the pack, stored relative to it (docs/voicepack-spec.md). */
  avatar: string | null;
}

export function createVoicesScreen(): HTMLElement {
  let draft: Draft | null = null;
  let known: Pack[] = [];

  const list = panel({
    title: "已安装的音色包",
    actions: [
      button({
        label: "刷新",
        glyph: "arrow-clockwise",
        small: true,
        onClick: () => void refreshVoices(),
      }),
      button({
        label: "添加目录…",
        glyph: "folder-open",
        small: true,
        onClick: () => {
          void pickFolder("选择音色包目录（LoRA）")
            .then((picked) => {
              if (picked !== null) startDraft(picked, true);
            })
            .catch((err: unknown) => toast(ipcMessage(err), "fail"));
        },
      }),
      button({
        label: "添加文件…",
        // Not primary: the empty state owns the one primary CTA on this screen, and
        // the panel header is a toolbar, not a call to action.
        glyph: "file-plus",
        small: true,
        onClick: () => {
          void pickFile("选择 *.speaker.safetensors 或参考音频", ["safetensors", "wav", "flac", "mp3"])
            .then((picked) => {
              if (picked !== null) startDraft(picked, false);
            })
            .catch((err: unknown) => toast(ipcMessage(err), "fail"));
        },
      }),
    ],
  });

  const draftHost = el("div", { class: "draft-host" });

  function startDraft(path: string, isFolder: boolean): void {
    const raw = baseName(path);
    const stem = isFolder ? raw : raw.replace(/\.speaker\.safetensors$/i, "").replace(/\.[a-z0-9]+$/i, "");
    draft = {
      path,
      kind: inferKind(path, isFolder),
      id: slugify(stem),
      name: stem,
      character: "",
      avatar: null,
      languages: "ja",
      engine: "irodori",
    };
    renderDraft();
  }

  function renderDraft(): void {
    if (draft === null) {
      fill(draftHost);
      return;
    }
    const current = draft;

    const idInput = el("input", {
      class: "input input--mono",
      id: "draft-id",
      type: "text",
      value: current.id,
      spellcheck: "false",
      autocapitalize: "off",
      oninput: (ev: Event) => {
        current.id = (ev.target as HTMLInputElement).value.trim();
      },
    });
    const nameInput = el("input", {
      class: "input",
      id: "draft-name",
      type: "text",
      value: current.name,
      oninput: (ev: Event) => {
        current.name = (ev.target as HTMLInputElement).value;
      },
    });
    const characterInput = el("input", {
      class: "input",
      id: "draft-character",
      type: "text",
      value: current.character,
      placeholder: "留空则用名称",
      oninput: (ev: Event) => {
        current.character = (ev.target as HTMLInputElement).value;
      },
    });
    const langInput = el("input", {
      class: "input input--mono",
      id: "draft-lang",
      type: "text",
      value: current.languages,
      spellcheck: "false",
      oninput: (ev: Event) => {
        current.languages = (ev.target as HTMLInputElement).value;
      },
    });
    const engineInput = el("input", {
      class: "input input--mono",
      id: "draft-engine",
      type: "text",
      value: current.engine,
      spellcheck: "false",
      oninput: (ev: Event) => {
        current.engine = (ev.target as HTMLInputElement).value.trim();
      },
    });
    const kindChip = el(
      "span",
      { class: "draft__kind" },
      chip(KIND_LABEL[current.kind], "accent", "waveform"),
    );
    const kindHint = el("p", { class: "draft__hint", text: KIND_HINT[current.kind] });
    const kindSelect = el(
      "select",
      {
        class: "input",
        id: "draft-kind",
        onchange: (ev: Event) => {
          current.kind = (ev.target as HTMLSelectElement).value as PackKind;
          // The two dependent nodes are updated in place: re-rendering the form here
          // would take focus off the select the user just used.
          fill(kindChip, chip(KIND_LABEL[current.kind], "accent", "waveform"));
          kindHint.textContent = KIND_HINT[current.kind];
        },
      },
      (Object.keys(KIND_LABEL) as PackKind[]).map((kind) =>
        el("option", { value: kind, selected: kind === current.kind, text: KIND_LABEL[kind] }),
      ),
    );

    // A picker, not a text field: the value stored on the pack is produced by the
    // import (which copies the file into the data dir), so a hand-typed path would be
    // a path this app never validated and the presenter would silently show nothing.
    const avatarValue = el("span", { class: "draft__avatarpath" });
    const avatarField = el("div", { class: "draft__avatar", id: "draft-avatar" });

    function renderAvatar(): void {
      avatarValue.textContent = current.avatar ?? "未设置";
      fill(
        avatarField,
        avatarValue,
        button({
          label: current.avatar === null ? "选择图片…" : "换一张",
          glyph: "file-plus",
          small: true,
          onClick: () => {
            void pickFile("选择头像图片", ["png", "jpg", "jpeg", "webp", "bmp"])
              .then(async (picked) => {
                if (picked === null) return;
                current.avatar = await importAvatar(picked, current.path);
                renderAvatar();
              })
              .catch((err: unknown) => toast(ipcMessage(err), "fail"));
          },
        }),
        current.avatar === null
          ? null
          : button({
              glyph: "x",
              name: "清除头像",
              title: "清除",
              small: true,
              kind: "quiet",
              onClick: () => {
                current.avatar = null;
                renderAvatar();
              },
            }),
      );
    }
    renderAvatar();

    const errorBox = el("div", { class: "draft__error" });

    function submit(): void {
      const id = current.id;
      const languages = current.languages
        .split(/[,，\s]+/)
        .map((lang) => lang.trim())
        .filter((lang) => lang.length > 0);

      const problems: string[] = [];
      if (!/^[a-z0-9][a-z0-9._-]*$/.test(id)) {
        problems.push("id 只能用小写字母、数字、点、下划线和短横线，且不能以符号开头。");
      }
      if (known.some((pack) => pack.id === id)) problems.push(`已经有一个叫 ${id} 的音色包了。`);
      if (current.name.trim().length === 0) problems.push("名称不能为空。");
      if (languages.length === 0) problems.push("至少写一种语言，例如 ja。");
      if (current.engine.length === 0) problems.push("引擎不能为空，Irodori 引擎填 irodori。");

      if (problems.length > 0) {
        fill(
          errorBox,
          note("fail", "还差一点", el("ul", { class: "draft__problems" }, problems.map((text) => el("li", { text })))),
        );
        return;
      }

      const pack: Pack = {
        id,
        name: current.name.trim(),
        kind: current.kind,
        path: current.path,
        engine: current.engine,
        languages,
        character: current.character.trim() === "" ? null : current.character.trim(),
        avatar: current.avatar,
      };

      void registerPack(pack)
        .then(async () => {
          draft = null;
          renderDraft();
          toast(`已登记音色包 ${pack.id}`, "ok");
          await refreshVoices();
          // The Setup screen counts registered packs, so it must not keep the old number.
          void refreshInventory();
        })
        .catch((err: unknown) => {
          fill(errorBox, note("fail", "写入 config.json 失败", el("p", { text: ipcMessage(err) })));
        });
    }

    fill(
      draftHost,
      el(
        "section",
        { class: "draft" },
        el(
          "header",
          { class: "draft__head" },
          el("h3", { class: "draft__title", text: "登记这个音色包" }),
          kindChip,
        ),
        el("div", { class: "draft__path" }, pathText(current.path, 72), openButton(current.path)),
        kindHint,
        el(
          "div",
          { class: "draft__grid" },
          field("draft-id", "id（调用时用这个）", idInput, "voicePackId，写进 POST /api/speak 的那个值。"),
          field("draft-name", "名称", nameInput),
          field("draft-character", "角色名", characterInput, "字幕弹窗上显示的说话人。"),
          field("draft-avatar", "头像", avatarField, "字幕弹窗左侧的那张脸；会复制进音色包自己的目录。"),
          field("draft-lang", "语言", langInput, "逗号分隔，例如 ja 或 ja,zh。"),
          field("draft-engine", "引擎", engineInput),
          field("draft-kind", "类型", kindSelect, "已按路径自动判断，不对可以改。"),
        ),
        errorBox,
        el(
          "div",
          { class: "draft__actions" },
          button({ label: "登记", kind: "primary", glyph: "check", onClick: submit }),
          button({
            label: "取消",
            kind: "quiet",
            onClick: () => {
              draft = null;
              renderDraft();
            },
          }),
        ),
      ),
    );
  }

  function packRow(pack: Pack): HTMLElement {
    const actions = el("div", { class: "pack__actions" });

    function renderActions(confirming: boolean): void {
      fill(
        actions,
        confirming
          ? [
              button({
                label: "确认移除",
                kind: "danger",
                glyph: "trash",
                small: true,
                onClick: () => {
                  void removePack(pack.id)
                    .then(async () => {
                      toast(`已移除 ${pack.id}`, "ok");
                      await refreshVoices();
                    })
                    .catch((err: unknown) => {
                      renderActions(false);
                      toast(`移除失败：${ipcMessage(err)}`, "fail");
                    });
                },
              }),
              button({ label: "算了", kind: "quiet", small: true, onClick: () => renderActions(false) }),
            ]
          : button({
              glyph: "trash",
              name: `移除 ${pack.id}`,
              title: "从 config.json 中移除",
              small: true,
              kind: "quiet",
              onClick: () => renderActions(true),
            }),
      );
    }
    renderActions(false);

    return el(
      "li",
      { class: "pack" },
      icon("waveform", "pack__icon"),
      el(
        "div",
        { class: "pack__main" },
        el(
          "div",
          { class: "pack__head" },
          el("h3", { class: "pack__name", text: pack.character ?? pack.name }),
          el("code", { class: "pack__id", dir: "ltr", text: pack.id }),
          chip(KIND_LABEL[pack.kind], "accent"),
        ),
        el(
          "div",
          { class: "pack__meta" },
          pack.character === null || pack.character === undefined
            ? null
            : el("span", { class: "pack__metaitem", text: `名称 ${pack.name}` }),
          el("span", { class: "pack__metaitem", text: `引擎 ${pack.engine === "" ? "未标注" : pack.engine}` }),
          el("span", {
            class: "pack__metaitem",
            text: `语言 ${pack.languages.length === 0 ? "未标注" : pack.languages.join(" / ")}`,
          }),
          pack.avatar === null || pack.avatar === undefined
            ? null
            : el("span", { class: "pack__metaitem", text: `头像 ${pack.avatar}` }),
        ),
        el("div", { class: "pack__path" }, pathText(pack.path, 64), openButton(pack.path)),
      ),
      actions,
    );
  }

  function renderList(packs: Pack[] | null): void {
    if (packs === null) {
      fill(
        list.body,
        el(
          "div",
          { class: "skeletons", "aria-hidden": "true" },
          [1, 2].map(() => el("div", { class: "skeleton" })),
        ),
      );
      return;
    }
    known = packs;

    if (packs.length === 0) {
      fill(
        list.body,
        emptyState({
          glyph: "microphone-stage",
          title: "还没有音色包",
          lines: [
            el("p", {
              text: "LoRA 目录、单个 *.speaker.safetensors，或者一段参考音频，三种都能直接登记。",
            }),
          ],
          actions: [
            button({
              label: "添加目录…",
              glyph: "folder-open",
              onClick: () => {
                void pickFolder("选择音色包目录（LoRA）")
                  .then((picked) => {
                    if (picked !== null) startDraft(picked, true);
                  })
                  .catch((err: unknown) => toast(ipcMessage(err), "fail"));
              },
            }),
            button({
              label: "添加文件…",
              kind: "primary",
              glyph: "file-plus",
              onClick: () => {
                void pickFile("选择 *.speaker.safetensors 或参考音频", [
                  "safetensors",
                  "wav",
                  "flac",
                  "mp3",
                ])
                  .then((picked) => {
                    if (picked !== null) startDraft(picked, false);
                  })
                  .catch((err: unknown) => toast(ipcMessage(err), "fail"));
              },
            }),
            button({
              label: "先去部署引擎",
              glyph: "download-simple",
              kind: "quiet",
              // The router lives in main.ts; a screen asking to be left is a message,
              // not a dependency.
              onClick: (ev: MouseEvent) => navigate("deploy", ev),
            }),
          ],
        }),
      );
      return;
    }

    fill(list.body, el("ul", { class: "packs" }, packs.map(packRow)));
  }

  voices.subscribe(renderList);
  void refreshVoices();

  return el(
    "div",
    { class: "screen" },
    el(
      "header",
      { class: "screen__head" },
      el(
        "div",
        { class: "screen__titles" },
        el("h1", { class: "screen__title", tabindex: "-1", text: "音色" }),
      ),
    ),
    draftHost,
    list.root,
  );
}
