import { useEffect, useMemo, useState } from "react";
import { msg, t } from "../i18n";
import {
  boldWeightFor,
  buildTerminalFontFamily,
  currentFontPlatform,
  isFontAvailable,
  presetsFor,
  terminalFontWeights,
  terminalTextRenderings,
  type TerminalFontWeight,
  type TerminalTextRendering,
} from "../features/settings/terminalFonts";

const CUSTOM = "__custom__";

const weightLabels: Record<TerminalFontWeight, string> = {
  normal: msg("常规"),
  "500": msg("中等"),
  "600": msg("半粗"),
};

const renderingLabels: Record<TerminalTextRendering, string> = {
  auto: msg("自动：Windows 和 macOS 使用清晰模式"),
  sharp: msg("清晰优先：不透明背景，启用 ClearType"),
  transparent: msg("透明优先：保留玻璃效果"),
};

/** Font family, weight and text rendering for the terminal, with a live preview. */
export function TerminalFontSettings({
  fontFamily,
  fontWeight,
  fontSize,
  textRendering,
  onFontFamilyChange,
  onFontWeightChange,
  onTextRenderingChange,
}: {
  fontFamily: string;
  fontWeight: TerminalFontWeight;
  fontSize: number;
  textRendering: TerminalTextRendering;
  onFontFamilyChange: (family: string) => void;
  onFontWeightChange: (weight: TerminalFontWeight) => void;
  onTextRenderingChange: (mode: TerminalTextRendering) => void;
}) {
  const platform = currentFontPlatform();
  const presets = useMemo(() => presetsFor(platform), [platform]);
  const installed = useMemo(
    () => new Map(presets.map((preset) => [preset.family, preset.bundled?.includes(platform) || isFontAvailable(preset.family)])),
    [presets, platform],
  );
  const isPreset = presets.some((preset) => preset.family === fontFamily);
  const [customOpen, setCustomOpen] = useState(() => fontFamily !== "" && !isPreset);
  const [customDraft, setCustomDraft] = useState(isPreset ? "" : fontFamily);

  useEffect(() => {
    if (fontFamily !== "" && !presets.some((preset) => preset.family === fontFamily)) {
      setCustomOpen(true);
      setCustomDraft(fontFamily);
    }
  }, [fontFamily, presets]);

  const selectValue = customOpen ? CUSTOM : fontFamily;
  const selectedPreset = presets.find((preset) => preset.family === fontFamily);
  const customMissing =
    customOpen && fontFamily !== "" && !fontFamily.includes(",") && !isFontAvailable(fontFamily);

  function commitCustom() {
    onFontFamilyChange(customDraft);
  }

  return (
    <>
      <label className="settings-select">
        <span>{t("字体")}</span>
        <select
          value={selectValue}
          onChange={(event) => {
            const next = event.target.value;
            if (next === CUSTOM) {
              setCustomOpen(true);
              setCustomDraft(isPreset ? "" : fontFamily);
              return;
            }
            setCustomOpen(false);
            onFontFamilyChange(next);
          }}
        >
          <option value="">{t("系统默认")}</option>
          {presets.map((preset) => (
            <option key={preset.family} value={preset.family}>
              {installed.get(preset.family) ? preset.family : t("{0}（未安装）", preset.family)}
            </option>
          ))}
          <option value={CUSTOM}>{t("自定义…")}</option>
        </select>
      </label>

      {customOpen && (
        <label className="settings-select">
          <span>{t("字体名称")}</span>
          <input
            type="text"
            value={customDraft}
            spellCheck={false}
            placeholder={t("例如：Fira Code, Sarasa Mono SC")}
            onChange={(event) => setCustomDraft(event.target.value)}
            onBlur={commitCustom}
            onKeyDown={(event) => {
              if (event.key === "Enter") event.currentTarget.blur();
            }}
          />
        </label>
      )}

      {(selectedPreset || customMissing) && (
        <p className="settings-hint">
          {selectedPreset && t(selectedPreset.note)}
          {selectedPreset && !installed.get(selectedPreset.family) && ` ${t("未检测到该字体，安装前会使用默认字体。")}`}
          {customMissing && t("未检测到该字体，安装前会使用默认字体。")}
        </p>
      )}

      <label className="settings-select">
        <span>{t("字重")}</span>
        <select
          value={fontWeight}
          onChange={(event) => onFontWeightChange(event.target.value as TerminalFontWeight)}
        >
          {terminalFontWeights.map((weight) => (
            <option key={weight} value={weight}>
              {t(weightLabels[weight])}
            </option>
          ))}
        </select>
      </label>

      <label className="settings-select">
        <span>{t("文字渲染")}</span>
        <select
          value={textRendering}
          onChange={(event) => onTextRenderingChange(event.target.value as TerminalTextRendering)}
        >
          {terminalTextRenderings.map((mode) => (
            <option key={mode} value={mode}>
              {t(renderingLabels[mode])}
            </option>
          ))}
        </select>
      </label>
      <p className="settings-hint">
        {t("透明背景上的文字只能用灰度抗锯齿，容易发虚，浅色主题下尤其明显；清晰模式让终端区域使用主题的实底颜色。")}
      </p>

      <div
        className="settings-font-preview"
        aria-label={t("字体预览")}
        style={{
          fontFamily: buildTerminalFontFamily(fontFamily, platform),
          fontSize,
          fontWeight,
        }}
      >
        <div>
          <span className="settings-font-preview-prompt">user@server</span>:~$ ls -la /var/log
        </div>
        <div>0O 1lI| {"{}"} [] () =&gt; != ~ `</div>
        <div>
          <b style={{ fontWeight: boldWeightFor(fontWeight) }}>Bold</b> 中文对齐测试 ABCD
        </div>
      </div>
    </>
  );
}
