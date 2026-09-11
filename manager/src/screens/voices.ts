// Voices: the voice pack registry, i.e. a view onto config.json's voicePacks.
//
// list_voices() answers whether the runtime is up or not (down = read from
// config.json directly), so this screen never has a "start the service first"
// state. Adding a pack is a two-step flow on purpose: pick, then confirm the
// metadata, because id / character / languages are what a caller will address the
// voice by and guessing them silently produces a pack nobody can use.
//
// The list is also the way in. Registering a pack settles four fields; everything else a
// pack decides about itself is a page of its own (`./voice`), shown in place of the list
// rather than in a window - so the rail keeps saying 音色, the back arrow goes back to the
// list, and Esc does the same. Leaving the screen closes that page: coming back to a rail
// entry and finding a sub-page you had forgotten about is a screen lying about where you
// are.

import { el, fill } from "../dom";
import { baseName, slugify } from "../format";
import { t } from "../i18n";
import {
  ipcMessage,
  importAvatar,
  packAvatar,
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
import { createVoiceDetail, kindLabel, PACK_KINDS } from "./voice";

function KIND_HINT(kind: PackKind): string {
  switch (kind) {
    case "lora-adapter":
      return t.voices.hintLoraAdapter;
    case "speaker-embedding":
      return t.voices.hintSpeakerEmbedding;
    case "reference-audio":
      return t.voices.hintReferenceAudio;
  }
}

/** A directory is a LoRA adapter; a lone file is an embedding unless it is audio.
 *  This is the same rule scripts/training/install_pack.py applies when it lays a
 *  trained pack down on disk. */
function inferKind(path: string, isFolder: boolean): PackKind {
  if (isFolder) return "lora-adapter";
  const lower = path.toLowerCase();
  if (/\.(wav|flac|mp3|ogg|m4a)$/.test(lower)) return "reference-audio";
  return "speaker-embedding";
}

/** Portraits already asked for, by pack id; `null` means there is not one to draw.
 *
 *  A `data:` URL *is* the bytes, so asking again on every re-render would re-read and
 *  re-encode a file for a row that has not changed. Dropped in the two places a portrait
 *  can stop matching: the pack page, which is where one is replaced, and 刷新. */
const portraits = new Map<string, string | null>();

/** The row's lead: the pack's portrait, or the first character of who it speaks as.
 *
 *  The fallback is the one the spec defines (`docs/voicepack-spec.md`, 头像的回退): the
 *  first character of `character`, else of `name` - never blank, never the id. One path
 *  covers all three ways there is no image, because the row draws the same thing for all
 *  three: no `avatar`, bytes the host refuses to inline, and bytes that do not decode. */
function portraitLead(pack: Pack): HTMLElement {
  const glyph = el("span", {
    class: "pack__portrait pack__portrait--glyph",
    // Decorative: the name it is the first character of is on the same row.
    "aria-hidden": "true",
    text: [...(pack.character ?? pack.name).trim()][0],
  });

  /** The portrait itself. An `onerror` rather than a check up front: the host can only
   *  promise the bytes exist, and bytes a decoder rejects are as absent as no file. */
  function image(url: string): HTMLImageElement {
    const img = el("img", { class: "pack__portrait", src: url, alt: "" });
    img.addEventListener("error", () => {
      portraits.set(pack.id, null);
      img.replaceWith(glyph);
    });
    return img;
  }

  if (pack.avatar === null || pack.avatar === undefined) return glyph;
  const cached = portraits.get(pack.id);
  if (cached !== undefined) return cached === null ? glyph : image(cached);

  // The glyph is already the right answer, so the bytes are fetched after the row is on
  // screen and swapped in when they arrive rather than holding up the list.
  void packAvatar(pack.id)
    .then((url) => {
      portraits.set(pack.id, url);
      if (url !== null) glyph.replaceWith(image(url));
    })
    .catch(() => portraits.set(pack.id, null));
  return glyph;
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
    title: t.voices.listTitle,
    actions: [
      button({
        label: t.voices.refresh,
        glyph: "arrow-clockwise",
        small: true,
        // Portraits go too: 刷新 is the user saying the list may be stale, and a file
        // replaced outside this app is the one change the cache cannot see by itself.
        onClick: () => {
          portraits.clear();
          void refreshVoices();
        },
      }),
      button({
        label: t.voices.addFolder,
        glyph: "folder-open",
        small: true,
        onClick: () => {
          void pickFolder(t.voices.pickFolderDialog)
            .then((picked) => {
              if (picked !== null) startDraft(picked, true);
            })
            .catch((err: unknown) => toast(ipcMessage(err), "fail"));
        },
      }),
      button({
        label: t.voices.addFile,
        // Not primary: the empty state owns the one primary CTA on this screen, and
        // the panel header is a toolbar, not a call to action.
        glyph: "file-plus",
        small: true,
        onClick: () => {
          void pickFile(t.voices.pickFileDialog, ["safetensors", "wav", "flac", "mp3"])
            .then((picked) => {
              if (picked !== null) startDraft(picked, false);
            })
            .catch((err: unknown) => toast(ipcMessage(err), "fail"));
        },
      }),
    ],
  });

  const draftHost = el("div", { class: "draft-host" });

  // The list side and the pack side of this screen, one visible at a time. Two hosts rather
  // than one re-rendered node: the detail page owns focus, a scroll position and an in-flight
  // 试听, and rebuilding it whenever the pack list refreshes would throw all three away.
  const listHost = el("div", { class: "screen__cards" }, draftHost, list.root);
  const detailHost = el("div");
  let openId: string | null = null;

  function openPack(pack: Pack): void {
    openId = pack.id;
    fill(detailHost, createVoiceDetail(pack, closePack));
    listHost.hidden = true;
  }

  function closePack(): void {
    const was = openId;
    if (was === null) return;
    openId = null;
    // The pack page is the one place in the app a portrait is replaced, so leaving it is
    // when this row's cached image stops being trustworthy.
    portraits.delete(was);
    fill(detailHost);
    listHost.hidden = false;
    // Back to the row it came from, not to the top of the page.
    const selector = `.pack__configure[data-pack="${CSS.escape(was)}"]`;
    list.root.querySelector<HTMLElement>(selector)?.focus();
  }

  /** The way into a pack's own page. `data-pack` is how `closePack` finds this button
   *  again to put focus back on it. */
  function configure(pack: Pack): HTMLElement {
    const control = button({
      label: t.voices.configure,
      glyph: "gear",
      small: true,
      onClick: () => openPack(pack),
    });
    control.classList.add("pack__configure");
    control.setAttribute("data-pack", pack.id);
    return control;
  }

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
      placeholder: t.voices.characterPlaceholder,
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
      chip(kindLabel(current.kind), "accent", "waveform"),
    );
    const kindHint = el("p", { class: "draft__hint", text: KIND_HINT(current.kind) });
    const kindSelect = el(
      "select",
      {
        class: "input",
        id: "draft-kind",
        onchange: (ev: Event) => {
          current.kind = (ev.target as HTMLSelectElement).value as PackKind;
          // The two dependent nodes are updated in place: re-rendering the form here
          // would take focus off the select the user just used.
          fill(kindChip, chip(kindLabel(current.kind), "accent", "waveform"));
          kindHint.textContent = KIND_HINT(current.kind);
        },
      },
      PACK_KINDS.map((kind) =>
        el("option", { value: kind, selected: kind === current.kind, text: kindLabel(kind) }),
      ),
    );

    // A picker, not a text field: the value stored on the pack is produced by the
    // import (which copies the file into the data dir), so a hand-typed path would be
    // a path this app never validated and the presenter would silently show nothing.
    const avatarValue = el("span", { class: "draft__avatarpath" });
    const avatarField = el("div", { class: "draft__avatar", id: "draft-avatar" });

    function renderAvatar(): void {
      avatarValue.textContent = current.avatar ?? t.common.notSet;
      fill(
        avatarField,
        avatarValue,
        button({
          label: current.avatar === null ? t.voices.avatarPick : t.voices.avatarChange,
          glyph: "file-plus",
          small: true,
          onClick: () => {
            void pickFile(t.voices.avatarPickDialog, ["png", "jpg", "jpeg", "webp", "bmp"])
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
              name: t.voices.avatarClear,
              title: t.common.clear,
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
        problems.push(t.voices.errBadId);
      }
      if (known.some((pack) => pack.id === id)) problems.push(t.voices.errDuplicateId(id));
      if (current.name.trim().length === 0) problems.push(t.voices.errNameEmpty);
      if (languages.length === 0) problems.push(t.voices.errNoLanguage);
      if (current.engine.length === 0) problems.push(t.voices.errEngineEmpty);

      if (problems.length > 0) {
        fill(
          errorBox,
          note("fail", t.voices.validationTitle, el("ul", { class: "draft__problems" }, problems.map((text) => el("li", { text })))),
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
          toast(t.voices.registeredToast(pack.id), "ok");
          await refreshVoices();
          // The Setup screen counts registered packs, so it must not keep the old number.
          void refreshInventory();
        })
        .catch((err: unknown) => {
          fill(errorBox, note("fail", t.voices.writeFailed, el("p", { text: ipcMessage(err) })));
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
          el("h3", { class: "draft__title", text: t.voices.draftTitle }),
          kindChip,
        ),
        el("div", { class: "draft__path" }, pathText(current.path, 72), openButton(current.path)),
        kindHint,
        el(
          "div",
          { class: "draft__grid" },
          field("draft-id", t.voices.idLabel, idInput, t.voices.idHint),
          field("draft-name", t.voices.nameLabel, nameInput),
          field("draft-character", t.voices.characterLabel, characterInput, t.voices.characterHint),
          field("draft-avatar", t.voices.avatarLabel, avatarField, t.voices.avatarHint),
          field("draft-lang", t.voices.langLabel, langInput, t.voices.langHint),
          field("draft-engine", t.voices.engineLabel, engineInput),
          field("draft-kind", t.voices.kindLabel, kindSelect, t.voices.kindAutoHint),
        ),
        errorBox,
        el(
          "div",
          { class: "draft__actions" },
          button({ label: t.voices.register, kind: "primary", glyph: "check", onClick: submit }),
          button({
            label: t.voices.cancel,
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
                label: t.voices.confirmRemove,
                kind: "danger",
                glyph: "trash",
                small: true,
                onClick: () => {
                  void removePack(pack.id)
                    .then(async () => {
                      toast(t.voices.removedToast(pack.id), "ok");
                      await refreshVoices();
                    })
                    .catch((err: unknown) => {
                      renderActions(false);
                      toast(t.voices.removeFailedToast(ipcMessage(err)), "fail");
                    });
                },
              }),
              button({ label: t.voices.cancel, kind: "quiet", small: true, onClick: () => renderActions(false) }),
            ]
          : [
              configure(pack),
              button({
                glyph: "trash",
                name: t.common.removeTag(pack.id),
                title: t.voices.removeTitle,
                small: true,
                kind: "quiet",
                onClick: () => renderActions(true),
              }),
            ],
      );
    }
    renderActions(false);

    return el(
      "li",
      { class: "pack" },
      portraitLead(pack),
      el(
        "div",
        { class: "pack__main" },
        el(
          "div",
          { class: "pack__head" },
          el("h3", { class: "pack__name", text: pack.character ?? pack.name }),
          el("code", { class: "pack__id", dir: "ltr", text: pack.id }),
          chip(kindLabel(pack.kind), "accent"),
        ),
        el(
          "div",
          { class: "pack__meta" },
          pack.character === null || pack.character === undefined
            ? null
            : el("span", { class: "pack__metaitem", text: t.voices.metaName(pack.name) }),
          el("span", {
            class: "pack__metaitem",
            text: t.voices.metaEngine(pack.engine === "" ? t.voices.unspecified : pack.engine),
          }),
          el("span", {
            class: "pack__metaitem",
            text: t.voices.metaLangs(pack.languages.length === 0 ? t.voices.unspecified : pack.languages.join(" / ")),
          }),
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
    // A pack removed under an open detail page - by the tray, by a hand-edit, by this very
    // screen - leaves that page describing something that is not there.
    if (openId !== null && !packs.some((pack) => pack.id === openId)) closePack();

    if (packs.length === 0) {
      fill(
        list.body,
        emptyState({
          glyph: "microphone-stage",
          title: t.voices.emptyTitle,
          lines: [
            el("p", {
              text: t.voices.emptyBody,
            }),
          ],
          actions: [
            button({
              label: t.voices.addFolder,
              glyph: "folder-open",
              onClick: () => {
                void pickFolder(t.voices.pickFolderDialog)
                  .then((picked) => {
                    if (picked !== null) startDraft(picked, true);
                  })
                  .catch((err: unknown) => toast(ipcMessage(err), "fail"));
              },
            }),
            button({
              label: t.voices.addFile,
              kind: "primary",
              glyph: "file-plus",
              onClick: () => {
                void pickFile(t.voices.pickFileDialog, [
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
              label: t.voices.goDeploy,
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

  // Leaving 音色 closes the pack page. The shell only hides this screen's element, so
  // without this the sub-page would still be there on the next visit - a rail entry whose
  // content is not what the rail says it is.
  document.addEventListener("app:navigate", (ev: Event) => {
    const { to } = (ev as CustomEvent<{ to: string }>).detail;
    if (to !== "voices") closePack();
  });

  return el(
    "div",
    { class: "screen" },
    el(
      "header",
      { class: "screen__head" },
      el(
        "div",
        { class: "screen__titles" },
        el("h1", { class: "screen__title", tabindex: "-1", text: t.voices.title }),
      ),
    ),
    listHost,
    detailHost,
  );
}
