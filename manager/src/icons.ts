// Phosphor's SVG assets, inlined at build time. Importing each glyph explicitly is
// what keeps the bundle at the ~30 icons this app actually draws instead of
// pulling an icon font or a 1400-file catalogue into an installer.
//
// The assets already carry `fill="currentColor"` and a 256x256 viewBox, so colour
// comes from the surrounding text colour and size comes from CSS.

import arrowClockwise from "@phosphor-icons/core/assets/regular/arrow-clockwise.svg?raw";
import arrowDown from "@phosphor-icons/core/assets/regular/arrow-down.svg?raw";
import arrowLeft from "@phosphor-icons/core/assets/regular/arrow-left.svg?raw";
import arrowSquareOut from "@phosphor-icons/core/assets/regular/arrow-square-out.svg?raw";
import bookOpenText from "@phosphor-icons/core/assets/regular/book-open-text.svg?raw";
import caretDown from "@phosphor-icons/core/assets/regular/caret-down.svg?raw";
import caretRight from "@phosphor-icons/core/assets/regular/caret-right.svg?raw";
import check from "@phosphor-icons/core/assets/regular/check.svg?raw";
import checkCircle from "@phosphor-icons/core/assets/regular/check-circle.svg?raw";
import circleDashed from "@phosphor-icons/core/assets/regular/circle-dashed.svg?raw";
import code from "@phosphor-icons/core/assets/regular/code.svg?raw";
import copy from "@phosphor-icons/core/assets/regular/copy.svg?raw";
import cpu from "@phosphor-icons/core/assets/regular/cpu.svg?raw";
import database from "@phosphor-icons/core/assets/regular/database.svg?raw";
import downloadSimple from "@phosphor-icons/core/assets/regular/download-simple.svg?raw";
import fileCode from "@phosphor-icons/core/assets/regular/file-code.svg?raw";
import filePlus from "@phosphor-icons/core/assets/regular/file-plus.svg?raw";
import folderOpen from "@phosphor-icons/core/assets/regular/folder-open.svg?raw";
import graphicsCard from "@phosphor-icons/core/assets/regular/graphics-card.svg?raw";
import hardDrives from "@phosphor-icons/core/assets/regular/hard-drives.svg?raw";
import info from "@phosphor-icons/core/assets/regular/info.svg?raw";
import key from "@phosphor-icons/core/assets/regular/key.svg?raw";
import magicWand from "@phosphor-icons/core/assets/regular/magic-wand.svg?raw";
import microphoneStage from "@phosphor-icons/core/assets/regular/microphone-stage.svg?raw";
import plus from "@phosphor-icons/core/assets/regular/plus.svg?raw";
import play from "@phosphor-icons/core/assets/regular/play.svg?raw";
import pulse from "@phosphor-icons/core/assets/regular/pulse.svg?raw";
import recycle from "@phosphor-icons/core/assets/regular/recycle.svg?raw";
import sparkle from "@phosphor-icons/core/assets/regular/sparkle.svg?raw";
import spinnerGap from "@phosphor-icons/core/assets/regular/spinner-gap.svg?raw";
import stop from "@phosphor-icons/core/assets/regular/stop.svg?raw";
import terminalWindow from "@phosphor-icons/core/assets/regular/terminal-window.svg?raw";
import trash from "@phosphor-icons/core/assets/regular/trash.svg?raw";
import warning from "@phosphor-icons/core/assets/regular/warning.svg?raw";
import warningCircle from "@phosphor-icons/core/assets/regular/warning-circle.svg?raw";
import waveform from "@phosphor-icons/core/assets/regular/waveform.svg?raw";
import x from "@phosphor-icons/core/assets/regular/x.svg?raw";

const SOURCES = {
  "arrow-clockwise": arrowClockwise,
  "arrow-down": arrowDown,
  "arrow-left": arrowLeft,
  "arrow-square-out": arrowSquareOut,
  "book-open-text": bookOpenText,
  "caret-down": caretDown,
  "caret-right": caretRight,
  check,
  "check-circle": checkCircle,
  "circle-dashed": circleDashed,
  code,
  copy,
  cpu,
  database,
  "download-simple": downloadSimple,
  "file-code": fileCode,
  "file-plus": filePlus,
  "folder-open": folderOpen,
  "graphics-card": graphicsCard,
  "hard-drives": hardDrives,
  info,
  key,
  "magic-wand": magicWand,
  "microphone-stage": microphoneStage,
  play,
  plus,
  pulse,
  recycle,
  sparkle,
  "spinner-gap": spinnerGap,
  stop,
  "terminal-window": terminalWindow,
  trash,
  warning,
  "warning-circle": warningCircle,
  waveform,
  x,
} as const;

export type IconName = keyof typeof SOURCES;

/** Parsed once per glyph, cloned per use: an installed tree draws the stage-state
 *  icons hundreds of times during one provision run, and re-parsing SVG text on
 *  every log line would be work with no output. */
const prototypes = new Map<IconName, SVGElement>();

export function icon(name: IconName, extraClass?: string): SVGElement {
  let proto = prototypes.get(name);
  if (proto === undefined) {
    const parsed = new DOMParser().parseFromString(SOURCES[name], "image/svg+xml").documentElement;
    parsed.setAttribute("aria-hidden", "true");
    parsed.setAttribute("focusable", "false");
    parsed.setAttribute("class", "icon");
    proto = parsed as unknown as SVGElement;
    prototypes.set(name, proto);
  }
  const node = proto.cloneNode(true) as SVGElement;
  if (extraClass !== undefined) node.setAttribute("class", `icon ${extraClass}`);
  return node;
}

/** The product mark, which is the one graphic in this app that is not an icon.
 *
 *  CJK corner brackets 「」 - the typographic sign that someone is speaking, which
 *  is exactly what this product turns text into - closing inward around a cleaved
 *  obsidian shard. Nothing radiates outward, because nothing leaves the machine.
 *  Three flat facets in three violets: the cut is the light, so the mark needs no
 *  gradient and survives being flattened to one colour.
 *
 *  Not a currentColor Phosphor glyph: the facets are three fixed values, and the
 *  brackets are ink while the core is accent. `manager/src-tauri/icons/mark.svg`
 *  holds the same geometry for the packaged app icons - change both or neither. */
const MARK = `<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg">
  <path d="M25 12H10V27" fill="none" stroke="#dadada" stroke-width="6.5" stroke-linecap="square"/>
  <path d="M39 52H54V37" fill="none" stroke="#dadada" stroke-width="6.5" stroke-linecap="square"/>
  <g transform="rotate(-8 32 32)">
    <path d="M32 13 20 27.5l3.5 16.5L32 51z" fill="#6c49df"/>
    <path d="M32 13l12 14.5-3.5 16.5L32 51z" fill="#8b6cef"/>
    <path d="M32 13l12 14.5-12 3.5z" fill="#a48bff"/>
  </g>
</svg>`;

export function brandMark(extraClass = "brand__mark"): SVGElement {
  const node = new DOMParser().parseFromString(MARK, "image/svg+xml").documentElement;
  node.setAttribute("aria-hidden", "true");
  node.setAttribute("focusable", "false");
  node.setAttribute("class", `icon ${extraClass}`);
  return node as unknown as SVGElement;
}
