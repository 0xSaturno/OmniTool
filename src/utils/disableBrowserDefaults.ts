// Always blocked — dangerous browser actions with no app equivalent.
const ALWAYS_BLOCKED_CTRL = new Set([
  "j", // downloads
  "n", // new window
  "p", // print
  "r", // reload
  "t", // new tab
  "u", // view source
  "w", // close tab / window  ← most dangerous
]);

// Blocked only when focus is outside a CodeMirror editor.
// CodeMirror uses all of these for editing features (search, replace, etc.).
const CM_PASSTHROUGH_CTRL = new Set([
  "d", // CM: select next occurrence  | browser: bookmark
  "f", // CM: search panel            | browser: find in page
  "g", // CM: find next               | browser: find next
  "h", // CM: find/replace panel      | browser: history
  "s", // CM: (potential save binding)| browser: save page
]);

const ALWAYS_BLOCKED_CTRL_SHIFT = new Set([
  "c", // inspect element
  "i", // devtools
  "j", // console
  "n", // incognito window
]);

function isInCodeMirror(target: EventTarget | null): boolean {
  return target instanceof Element && target.closest(".cm-editor") !== null;
}

export function disableBrowserDefaults(): void {
  document.addEventListener("contextmenu", (e) => e.preventDefault());

  // Capture phase — preempts browser defaults without stopping propagation,
  // so app-level handlers (CodeMirror keymaps, React events) still fire.
  document.addEventListener(
    "keydown",
    (e) => {
      if (!e.ctrlKey && !e.altKey) {
        if (e.key === "F1" /* || e.key === "F5" */) e.preventDefault();
        return;
      }

      if (e.ctrlKey && !e.altKey) {
        const key = e.key.toLowerCase();
        if (e.shiftKey) {
          if (ALWAYS_BLOCKED_CTRL_SHIFT.has(key)) e.preventDefault();
        } else {
          if (ALWAYS_BLOCKED_CTRL.has(key)) {
            e.preventDefault();
          } else if (CM_PASSTHROUGH_CTRL.has(key) && !isInCodeMirror(e.target)) {
            e.preventDefault();
          }
        }
      }

      if (e.altKey && !e.ctrlKey) {
        if (e.key === "ArrowLeft" || e.key === "ArrowRight") e.preventDefault();
      }
    },
    true,
  );
}
