import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { useLocation } from "react-router-dom";
import { LuArrowUp, LuArrowDown, LuFilePlus, LuFolderPlus, LuRefreshCw } from "react-icons/lu";
import StatusLog, { type LogEntry } from "../../components/shared/StatusLog";
import { useSettings } from "../../contexts/SettingsContext";
import { useRivetHookInstalled } from "../../utils/useRivetHook";
import styles from "./RivetManager.module.css";

type EntryKind = "bool" | "int" | "float" | "string" | "key" | "list";

interface RivetEntry {
  key: string;
  kind: EntryKind;
  value: unknown;
  comment: string;
}

interface RivetSection {
  name: string;
  entries: RivetEntry[];
}

interface RivetMod {
  path: string;
  enabled: boolean;
  exists: boolean;
}

interface RivetConfig {
  config_path: string;
  exists: boolean;
  game_running: boolean;
  sections: RivetSection[];
  mods: RivetMod[];
}

interface PendingEdit {
  section: string;
  key: string;
  value: unknown;
}

const ROUTE = "/tools/rivet-manager";
const MODS_TAB = "__mods__";
// Written by the hook but never read back, so an edit would not stick.
const READ_ONLY = new Set(["assets.disable_dstorage"]);
const SECTION_LABELS: Record<string, string> = { ddl: "DDL", renderdoc: "RenderDoc", hero_look: "Hero Look", log: "Logging" };

const CODE_TO_VK: Record<string, string> = {
  Escape: "ESCAPE", Backspace: "BACK", Enter: "RETURN", Space: "SPACE", Insert: "INSERT", Delete: "DELETE",
  Home: "HOME", End: "END", PageUp: "PRIOR", PageDown: "NEXT",
  ArrowLeft: "LEFT", ArrowUp: "UP", ArrowRight: "RIGHT", ArrowDown: "DOWN",
  Pause: "PAUSE", ScrollLock: "SCROLL", NumLock: "NUMLOCK", CapsLock: "CAPITAL", PrintScreen: "SNAPSHOT",
  ShiftLeft: "LSHIFT", ShiftRight: "RSHIFT", ControlLeft: "LCONTROL", ControlRight: "RCONTROL",
  AltLeft: "LMENU", AltRight: "RMENU",
  Backquote: "OEM_3", Minus: "OEM_MINUS", Equal: "OEM_PLUS", BracketLeft: "OEM_4", BracketRight: "OEM_6",
  Backslash: "OEM_5", Semicolon: "OEM_1", Quote: "OEM_7", Comma: "OEM_COMMA", Period: "OEM_PERIOD", Slash: "OEM_2",
  NumpadMultiply: "MULTIPLY", NumpadAdd: "ADD", NumpadSubtract: "SUBTRACT", NumpadDecimal: "DECIMAL",
  NumpadDivide: "DIVIDE", NumpadEnter: "RETURN",
};

/** Rivet Hook's virtual-key name for a KeyboardEvent.code. */
function vkName(code: string): string | null {
  if (/^F([1-9]|1\d|2[0-4])$/.test(code)) return code;
  const letter = /^Key([A-Z])$/.exec(code);
  if (letter) return letter[1];
  const digit = /^Digit(\d)$/.exec(code);
  if (digit) return digit[1];
  const numpad = /^Numpad(\d)$/.exec(code);
  if (numpad) return `NUMPAD${numpad[1]}`;
  return CODE_TO_VK[code] ?? null;
}

function sectionLabel(name: string): string {
  return SECTION_LABELS[name] ?? name.charAt(0).toUpperCase() + name.slice(1).replace(/_/g, " ");
}

function keyLabel(key: string): string {
  const words = key.split("_").map((w) => (["ddl", "dll", "id", "io"].includes(w) ? w.toUpperCase() : w));
  const text = words.join(" ");
  return text.charAt(0).toUpperCase() + text.slice(1);
}

/** Game-dir-relative with forward slashes, the way the hook's own entries look. */
function toGamePath(picked: string, gameDir: string): string {
  const abs = picked.replace(/\\/g, "/");
  const root = gameDir.replace(/\\/g, "/").replace(/\/+$/, "");
  return abs.toLowerCase().startsWith(`${root.toLowerCase()}/`) ? abs.slice(root.length + 1) : picked;
}

const same = (a: unknown, b: unknown) => JSON.stringify(a) === JSON.stringify(b);

export default function RivetManager() {
  const location = useLocation();
  const { settings, setSettingsOpen } = useSettings();
  const gameDir = settings.archivesDir;
  const rivetInstalled = useRivetHookInstalled(gameDir);
  const [config, setConfig] = useState<RivetConfig | null>(null);
  const [edits, setEdits] = useState<Map<string, PendingEdit>>(new Map());
  const [mods, setMods] = useState<RivetMod[]>([]);
  const [tab, setTab] = useState(MODS_TAB);
  const [busy, setBusy] = useState(false);
  const [log, setLog] = useState<LogEntry[]>([]);

  const originalEnabled = useMemo(() => config?.mods.filter((m) => m.enabled).map((m) => m.path) ?? [], [config]);
  const enabledPaths = mods.filter((m) => m.enabled).map((m) => m.path);
  const modsDirty = !same(enabledPaths, originalEnabled);
  const dirtyCount = edits.size + (modsDirty ? 1 : 0);
  const assets = config?.sections.find((s) => s.name === "assets");
  const sectionTabs = config?.sections.filter((s) => s.name !== "assets" && s.entries.length > 0) ?? [];

  function pushLog(type: LogEntry["type"], message: string) {
    setLog((prev) => [...prev, { type, message, ts: Date.now() }]);
  }

  async function load(quiet = false) {
    if (!gameDir) return;
    setBusy(true);
    try {
      const result = await invoke<RivetConfig>("read_rivet_config", { gameDir });
      setConfig(result);
      setMods(result.mods);
      setEdits(new Map());
      if (!quiet) pushLog("info", `Loaded ${result.config_path}`);
    } catch (e) {
      pushLog("error", String(e));
    } finally {
      setBusy(false);
    }
  }

  // Reload whenever the page is shown (the Stager may have installed a mod), unless edits are pending.
  useEffect(() => {
    if (location.pathname !== ROUTE || !gameDir) return;
    if (edits.size === 0 && !modsDirty) load(true);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [location.pathname, gameDir]);

  function valueOf(section: string, entry: RivetEntry): unknown {
    const edit = edits.get(`${section}.${entry.key}`);
    return edit ? edit.value : entry.value;
  }

  function setValue(section: string, entry: RivetEntry, value: unknown) {
    setEdits((prev) => {
      const next = new Map(prev);
      const id = `${section}.${entry.key}`;
      if (same(value, entry.value)) next.delete(id);
      else next.set(id, { section, key: entry.key, value });
      return next;
    });
  }

  function setModEnabled(path: string, on: boolean) {
    setMods((prev) => {
      const mod = prev.find((m) => m.path === path);
      if (!mod) return prev;
      const rest = prev.filter((m) => m.path !== path);
      return [...rest.filter((m) => m.enabled), { ...mod, enabled: on }, ...rest.filter((m) => !m.enabled)];
    });
  }

  function moveMod(index: number, delta: number) {
    setMods((prev) => {
      const enabled = prev.filter((m) => m.enabled);
      const j = index + delta;
      if (j < 0 || j >= enabled.length) return prev;
      [enabled[index], enabled[j]] = [enabled[j], enabled[index]];
      return [...enabled, ...prev.filter((m) => !m.enabled)];
    });
  }

  async function addMod(directory: boolean) {
    const picked = await open({
      directory,
      multiple: false,
      defaultPath: gameDir,
      filters: directory ? undefined : [{ name: "Stage", extensions: ["stage"] }],
    });
    if (typeof picked !== "string") return;
    const path = toGamePath(picked, gameDir);
    const existing = mods.find((m) => m.path.toLowerCase() === path.toLowerCase());
    if (existing) {
      setModEnabled(existing.path, true);
      return;
    }
    setMods((prev) => [
      ...prev.filter((m) => m.enabled),
      { path, enabled: true, exists: true },
      ...prev.filter((m) => !m.enabled),
    ]);
  }

  async function save() {
    setBusy(true);
    try {
      const path = await invoke<string>("write_rivet_config", {
        gameDir,
        edits: [...edits.values()],
        mods: modsDirty ? enabledPaths : null,
      });
      pushLog("success", `Saved ${dirtyCount} change(s) → ${path} (previous copy in rivet.toml.bak)`);
      await load(true);
    } catch (e) {
      pushLog("error", String(e));
    } finally {
      setBusy(false);
    }
  }

  function renderControl(section: string, entry: RivetEntry) {
    const value = valueOf(section, entry);
    const readOnly = READ_ONLY.has(`${section}.${entry.key}`);
    switch (entry.kind) {
      case "bool":
        return (
          <button
            type="button"
            role="switch"
            aria-checked={value === true}
            className={`${styles.switch} ${value === true ? styles.switchOn : ""}`}
            onClick={() => setValue(section, entry, value !== true)}
            disabled={readOnly}
          >
            <span className={styles.switchKnob} />
          </button>
        );
      case "int":
      case "float":
        return (
          <input
            type="number"
            className={`${styles.input} ${styles.numberInput}`}
            value={String(value)}
            step={entry.kind === "int" ? 1 : "any"}
            onChange={(e) => {
              const n = Number(e.target.value);
              if (e.target.value.trim() !== "" && Number.isFinite(n)) {
                setValue(section, entry, entry.kind === "int" ? Math.trunc(n) : n);
              }
            }}
            disabled={readOnly}
          />
        );
      case "key":
        return (
          <input
            className={`${styles.input} ${styles.keyInput}`}
            value={String(value)}
            placeholder="Press a key…"
            readOnly
            title="Click, then press the key to bind"
            onKeyDown={(e) => {
              if (e.key === "Tab") return;
              e.preventDefault();
              const vk = vkName(e.code);
              if (vk) setValue(section, entry, vk);
            }}
            disabled={readOnly}
          />
        );
      case "list":
        return (
          <textarea
            className={`${styles.input} ${styles.listInput}`}
            value={(value as string[]).join("\n")}
            placeholder="One entry per line"
            onChange={(e) =>
              setValue(section, entry, e.target.value.split(/\r?\n/).map((s) => s.trim()).filter(Boolean))
            }
            disabled={readOnly}
          />
        );
      default:
        return (
          <input
            className={styles.input}
            value={String(value)}
            onChange={(e) => setValue(section, entry, e.target.value)}
            disabled={readOnly}
          />
        );
    }
  }

  function renderEntries(section: string, entries: RivetEntry[]) {
    return (
      <div className={styles.entries}>
        {entries.map((entry) => {
          const dirty = edits.has(`${section}.${entry.key}`);
          const readOnly = READ_ONLY.has(`${section}.${entry.key}`);
          return (
            <div key={entry.key} className={`${styles.entry} ${dirty ? styles.entryDirty : ""}`}>
              <div className={styles.entryText}>
                <span className={styles.entryLabel}>{keyLabel(entry.key)}</span>
                {(entry.comment || readOnly) && (
                  <span className={styles.entryComment}>
                    {entry.comment}
                    {readOnly}
                  </span>
                )}
              </div>
              <div className={styles.entryControl}>{renderControl(section, entry)}</div>
            </div>
          );
        })}
      </div>
    );
  }

  function renderMods() {
    const enabled = mods.filter((m) => m.enabled);
    const disabled = mods.filter((m) => !m.enabled);
    return (
      <>
        {assets && renderEntries("assets", assets.entries.filter((e) => e.key !== "paths"))}

        <div className={styles.modHeader}>
          <div>
            <h3>Load order</h3>
            <p className={styles.helperText}>Loaded top to bottom: mods lower in the list override the ones above.</p>
          </div>
          <div className={styles.modActions}>
            <button className={styles.secondaryBtn} onClick={() => addMod(false)} disabled={busy}>
              <LuFilePlus /> Add .stage
            </button>
            <button className={styles.secondaryBtn} onClick={() => addMod(true)} disabled={busy}>
              <LuFolderPlus /> Add folder
            </button>
          </div>
        </div>

        {enabled.length === 0 && <p className={styles.emptyText}>No mods are loaded.</p>}
        <ul className={styles.modList}>
          {enabled.map((m, i) => (
            <li key={m.path} className={styles.modRow}>
              <button
                type="button"
                role="switch"
                aria-checked
                className={`${styles.switch} ${styles.switchOn}`}
                onClick={() => setModEnabled(m.path, false)}
                title="Stop loading this mod"
              >
                <span className={styles.switchKnob} />
              </button>
              <span className={styles.modIndex}>{i + 1}</span>
              <span className={styles.modPath}>{m.path}</span>
              {!m.exists && <span className={styles.missingBadge}>missing</span>}
              <button className={styles.iconBtn} onClick={() => moveMod(i, -1)} disabled={i === 0} title="Load earlier">
                <LuArrowUp />
              </button>
              <button
                className={styles.iconBtn}
                onClick={() => moveMod(i, 1)}
                disabled={i === enabled.length - 1}
                title="Load later (higher priority)"
              >
                <LuArrowDown />
              </button>
            </li>
          ))}
        </ul>

        {disabled.length > 0 && (
          <>
            <h3 className={styles.subHeading}>Not loaded</h3>
            <ul className={styles.modList}>
              {disabled.map((m) => (
                <li key={m.path} className={`${styles.modRow} ${styles.modRowOff}`}>
                  <button
                    type="button"
                    role="switch"
                    aria-checked={false}
                    className={styles.switch}
                    onClick={() => setModEnabled(m.path, true)}
                    title="Load this mod"
                  >
                    <span className={styles.switchKnob} />
                  </button>
                  <span className={styles.modPath}>{m.path}</span>
                  {!m.exists && <span className={styles.missingBadge}>missing</span>}
                </li>
              ))}
            </ul>
          </>
        )}
      </>
    );
  }

  const activeSection = sectionTabs.find((s) => s.name === tab);

  return (
    <div className={styles.page}>
      <div className={styles.header}>
        <h2 className={styles.title}>RivetHook Manager</h2>
        <span className={styles.subtitle}>Mods and settings in rivet.toml</span>
      </div>

      {!gameDir ? (
        <div className={styles.notice}>
          Set the game folder in Settings to find rivet.toml.
          <button className={styles.secondaryBtn} onClick={() => setSettingsOpen(true)}>
            Open Settings
          </button>
        </div>
      ) : (
        <>
          {rivetInstalled === false && (
            <div className={`${styles.notice} ${styles.noticeWarn}`}>
              Rivet Hook not detected: rivet_hook.dll is not in {gameDir}. Install it (hid.dll + rivet_hook.dll next
              to RiftApart.exe) for these settings and mods to load.
            </div>
          )}
          {config?.game_running && (
            <div className={`${styles.notice} ${styles.noticeWarn}`}>
              Rift Apart is running. Rivet Hook rewrites rivet.toml when the game closes, so saving is blocked
              until you close it.
            </div>
          )}
          {config && !config.exists && (
            <div className={styles.notice}>
              No rivet.toml at {config.config_path}. Rivet Hook creates it on first launch; saving here creates it
              with only the values you change.
            </div>
          )}

          {config && (
            <>
              <div className={styles.tabs}>
                {[{ name: MODS_TAB, label: "Mods" }, ...sectionTabs.map((s) => ({ name: s.name, label: sectionLabel(s.name) }))].map(
                  (t) => {
                    const dirty =
                      t.name === MODS_TAB
                        ? modsDirty || [...edits.values()].some((e) => e.section === "assets")
                        : [...edits.values()].some((e) => e.section === t.name);
                    return (
                      <button
                        key={t.name}
                        className={`${styles.tab} ${tab === t.name ? styles.active : ""}`}
                        onClick={() => setTab(t.name)}
                      >
                        {t.label}
                        {dirty && <span className={styles.tabDirty}>●</span>}
                      </button>
                    );
                  },
                )}
              </div>

              <section className={styles.pane}>
                {tab === MODS_TAB || !activeSection ? renderMods() : renderEntries(activeSection.name, activeSection.entries)}
              </section>
            </>
          )}

          <div className={styles.actions}>
            <button className={styles.runBtn} onClick={save} disabled={busy || dirtyCount === 0 || !config}>
              {busy ? "Working…" : `Save Changes (${dirtyCount})`}
            </button>
            <button className={styles.secondaryBtn} onClick={() => load()} disabled={busy}>
              <LuRefreshCw /> {dirtyCount > 0 ? "Discard & reload" : "Reload"}
            </button>
          </div>
        </>
      )}

      <StatusLog entries={log} />
    </div>
  );
}
