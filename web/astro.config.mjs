import { defineConfig } from "astro/config";
import tailwind from "@astrojs/tailwind";

// Locales served at:
//   /              -> en-US (default, no prefix)
//   /zh-CN/        -> Simplified Chinese
//   /zh-TW/        -> Traditional Chinese
//   /ja-JP/        -> Japanese
//   /ko-KR/        -> Korean
//   /es-ES/        -> Spanish
//   /pt-BR/        -> Portuguese (Brazil)
//   /de-DE/        -> German
//   /fr-FR/        -> French
//   /it-IT/        -> Italian
//   /ru-RU/        -> Russian
//   /ar-SA/        -> Arabic (RTL)
//   /hi-IN/        -> Hindi (Devanagari)
//   /vi-VN/        -> Vietnamese
//   /id-ID/        -> Indonesian
//   /tr-TR/        -> Turkish
export default defineConfig({
  site: "https://unterm.app",
  integrations: [tailwind()],
  i18n: {
    defaultLocale: "en-US",
    locales: [
      "en-US",
      "zh-CN",
      "zh-TW",
      "ja-JP",
      "ko-KR",
      "es-ES",
      "pt-BR",
      "de-DE",
      "fr-FR",
      "it-IT",
      "ru-RU",
      "ar-SA",
      "hi-IN",
      "vi-VN",
      "id-ID",
      "tr-TR",
    ],
    routing: {
      prefixDefaultLocale: false,
    },
  },
});
