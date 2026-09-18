import type { IndexProgress, ViewState } from "../../shared/types";
import { IndexProgressBar } from "./IndexProgressBar";

interface KeybindBarProps {
  /** How far the backend has got reading the sessions directory. */
  index: IndexProgress;
  view: ViewState;
  showHints?: boolean;
  onToggle?: () => void;
}

interface KeyHint {
  key: string;
  label: string;
}

const pickerKeys: KeyHint[] = [
  { key: "j/k", label: "nav" },
  { key: "Enter", label: "open" },
  { key: "/", label: "search" },
];

const listKeys: KeyHint[] = [
  { key: "j/k", label: "nav" },
  { key: "Enter", label: "detail" },
  { key: "e/c", label: "expand/collapse" },
  { key: "q", label: "sessions" },
];

const detailKeys: KeyHint[] = [
  { key: "j/k", label: "items" },
  { key: "Tab", label: "toggle" },
  { key: "q/Esc", label: "back" },
];

function getKeys(view: ViewState): KeyHint[] {
  switch (view) {
    case "picker":
      return pickerKeys;
    case "list":
      return listKeys;
    case "detail":
      return detailKeys;
  }
}

export function KeybindBar({ index, view, showHints = true, onToggle }: KeybindBarProps) {
  const keys = getKeys(view);
  return (
    <div className="keybind-bar">
      <IndexProgressBar progress={index} />
      <div className="keybind-bar__hints">
        {showHints &&
          keys.map((hint) => (
            <span key={hint.key} className="keybind-bar__item">
              <span className="keybind-bar__key">{hint.key}</span>
              <span className="keybind-bar__label">{hint.label}</span>
            </span>
          ))}
        {onToggle && (
          <button
            className="keybind-bar__toggle"
            onClick={onToggle}
            title={showHints ? "Hide keybinds" : "Show keybinds"}
          >
            ?
          </button>
        )}
      </div>
    </div>
  );
}
