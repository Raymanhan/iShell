import { useEffect, useMemo, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { Search } from "lucide-react";
import { rankEntries, type PaletteEntry } from "../features/palette/match";

export interface PaletteItem extends PaletteEntry {
  /** Shown on the right: what kind of item this is. */
  section: string;
  icon?: ReactNode;
  /** A shortcut shown on the right, e.g. "⌘D". */
  shortcut?: string;
  run: () => void;
}

/**
 * Quick access to servers, open tabs, snippets and app actions by typing.
 * Arrow keys move, Enter runs, Escape or a click outside closes.
 */
export function CommandPalette({ items, onClose }: { items: PaletteItem[]; onClose: () => void }) {
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState(0);
  const listRef = useRef<HTMLDivElement>(null);
  const results = useMemo(() => rankEntries(items, query), [items, query]);

  useEffect(() => setSelected(0), [query]);
  useEffect(() => {
    listRef.current
      ?.querySelector<HTMLElement>(`[data-index="${selected}"]`)
      ?.scrollIntoView({ block: "nearest" });
  }, [selected]);

  function run(item: PaletteItem | undefined) {
    if (!item) return;
    onClose();
    item.run();
  }

  function onKeyDown(event: KeyboardEvent<HTMLInputElement>) {
    if (event.nativeEvent.isComposing) return;
    if (event.key === "ArrowDown" || (event.ctrlKey && event.key === "n")) {
      event.preventDefault();
      setSelected((index) => (results.length ? (index + 1) % results.length : 0));
    } else if (event.key === "ArrowUp" || (event.ctrlKey && event.key === "p")) {
      event.preventDefault();
      setSelected((index) => (results.length ? (index - 1 + results.length) % results.length : 0));
    } else if (event.key === "Enter") {
      event.preventDefault();
      run(results[selected]);
    } else if (event.key === "Escape") {
      event.preventDefault();
      onClose();
    }
  }

  return (
    <div
      className="palette-backdrop"
      role="presentation"
      onPointerDown={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div className="command-palette" role="dialog" aria-modal="true" aria-label="命令面板">
        <label className="palette-search">
          <Search size={15} aria-hidden />
          <input
            autoFocus
            value={query}
            placeholder="搜索服务器、标签、片段或操作…"
            role="combobox"
            aria-expanded
            aria-controls="palette-results"
            aria-activedescendant={results[selected] ? `palette-${results[selected].id}` : undefined}
            onChange={(event) => setQuery(event.target.value)}
            onKeyDown={onKeyDown}
          />
        </label>
        <div className="palette-results" id="palette-results" role="listbox" ref={listRef}>
          {results.length === 0 && <div className="palette-empty">没有匹配的项目</div>}
          {results.map((item, index) => (
            <div
              key={item.id}
              id={`palette-${item.id}`}
              data-index={index}
              role="option"
              aria-selected={index === selected}
              className={`palette-item ${index === selected ? "selected" : ""}`}
              onPointerMove={() => setSelected(index)}
              onClick={() => run(item)}
            >
              <span className="palette-icon">{item.icon}</span>
              <span className="palette-text">
                <span className="palette-title">{item.title}</span>
                {item.detail && <span className="palette-detail">{item.detail}</span>}
              </span>
              {item.shortcut && <kbd>{item.shortcut}</kbd>}
              <span className="palette-section">{item.section}</span>
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}
