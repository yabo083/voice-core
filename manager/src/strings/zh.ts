// The Chinese dictionary: the source of truth the panel renders from.
//
// One file per domain under `strings/`, each carrying the domain's `zh` object and its
// English mirror typed against it. Composition is flat, so a call site reads
// `t.status.title` and the compiler guarantees the same key exists in English.

import { commonZh } from "./common";
import { deployZh } from "./deploy";
import { settingsZh } from "./settings";
import { statusZh } from "./status";
import { trainZh } from "./train";
import { voiceZh } from "./voice";
import { voicesZh } from "./voices";

export const zh = {
  common: commonZh,
  deploy: deployZh,
  settings: settingsZh,
  status: statusZh,
  train: trainZh,
  voice: voiceZh,
  voices: voicesZh,
};

/** The dictionary shape every language must satisfy. */
export type Dict = typeof zh;
