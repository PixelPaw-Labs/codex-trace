import { useEffect, useRef } from "react";

export function useKeyboard(keyMap: Record<string, () => void>) {
  const keyMapRef = useRef(keyMap);
  // Sync the latest keyMap from an effect rather than during render: writing to a
  // ref while rendering is what react(refs) flags.
  useEffect(() => {
    keyMapRef.current = keyMap;
  });

  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement;
      if (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.isContentEditable) {
        return;
      }
      const action = keyMapRef.current[e.key];
      if (action) {
        e.preventDefault();
        action();
      }
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, []);
}
