// The English dictionary.
//
// `Dict = typeof zh` makes a key present in Chinese but missing here a compile error,
// which is the only thing that keeps the two objects from drifting as screens change.

import { commonEn } from "./common";
import { deployEn } from "./deploy";
import { settingsEn } from "./settings";
import { statusEn } from "./status";
import { trainEn } from "./train";
import { voiceEn } from "./voice";
import { voicesEn } from "./voices";
import { type Dict } from "./zh";

export const en: Dict = {
  common: commonEn,
  deploy: deployEn,
  settings: settingsEn,
  status: statusEn,
  train: trainEn,
  voice: voiceEn,
  voices: voicesEn,
};
