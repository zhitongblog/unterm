// Central registry of all non-default locale dictionaries.
// Adding a new language only requires adding its JSON file here and an entry
// in locales.ts; all [lang] Astro routes pick it up automatically.

import zhCN from "./zh-CN.json";
import zhTW from "./zh-TW.json";
import jaJP from "./ja-JP.json";
import koKR from "./ko-KR.json";
import esES from "./es-ES.json";
import ptBR from "./pt-BR.json";
import deDE from "./de-DE.json";
import frFR from "./fr-FR.json";
import itIT from "./it-IT.json";
import ruRU from "./ru-RU.json";
import arSA from "./ar-SA.json";
import hiIN from "./hi-IN.json";
import viVN from "./vi-VN.json";
import idID from "./id-ID.json";
import trTR from "./tr-TR.json";

export const DICTS: Record<string, Record<string, string>> = {
  "zh-CN": zhCN,
  "zh-TW": zhTW,
  "ja-JP": jaJP,
  "ko-KR": koKR,
  "es-ES": esES,
  "pt-BR": ptBR,
  "de-DE": deDE,
  "fr-FR": frFR,
  "it-IT": itIT,
  "ru-RU": ruRU,
  "ar-SA": arSA,
  "hi-IN": hiIN,
  "vi-VN": viVN,
  "id-ID": idID,
  "tr-TR": trTR,
};

export const NON_DEFAULT_LANGS = Object.keys(DICTS);
