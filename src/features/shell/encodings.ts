import { msg, t } from "../../i18n";

/** Terminal encodings offered in the UI, by the names the backend returns. */
export const TERMINAL_ENCODINGS: { value: string; label: string }[] = [
  { value: "UTF-8", label: "UTF-8" },
  { value: "GBK", label: msg("GBK（简体中文）") },
  { value: "gb18030", label: msg("GB18030（简体中文）") },
  { value: "Big5", label: msg("Big5（繁體中文）") },
  { value: "Shift_JIS", label: msg("Shift_JIS（日本語）") },
  { value: "EUC-JP", label: msg("EUC-JP（日本語）") },
  { value: "EUC-KR", label: "EUC-KR（한국어）" },
  { value: "windows-1252", label: msg("Windows-1252 / Latin-1（西欧）") },
  { value: "ISO-8859-2", label: msg("ISO-8859-2（中欧）") },
  { value: "windows-1251", label: msg("Windows-1251（西里尔）") },
  { value: "KOI8-R", label: msg("KOI8-R（俄语）") },
];

export const DEFAULT_ENCODING = "UTF-8";

export function encodingLabel(value: string | null | undefined): string {
  const name = value || DEFAULT_ENCODING;
  const label = TERMINAL_ENCODINGS.find((item) => item.value.toLowerCase() === name.toLowerCase())?.label;
  return label ? t(label) : name;
}
