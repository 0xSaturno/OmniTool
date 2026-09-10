import { useCallback, useEffect, useMemo, useState, type ReactElement } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useLocation } from "react-router-dom";
import CodeMirror from "@uiw/react-codemirror";
import { json as jsonLang } from "@codemirror/lang-json";
import { vscodeDark } from "@uiw/codemirror-theme-vscode";
import FilePickerInput from "../../components/shared/FilePickerInput";
import SendToStagerModal from "../../components/shared/SendToStagerModal";
import StatusLog, { type LogEntry } from "../../components/shared/StatusLog";
import { deriveStagerTarget } from "../../utils/stagerTarget";
import styles from "./ArenaEditor.module.css";

const ZONE_FILTER = [{ name: "Zone", extensions: ["zone"] }];

/// Node types that shape how a wave plays out.
const PACING_TYPES = [
  "SpawnerScriptAction",
  "SunsetSpawnerFactoryScriptAction",
  "GameSpawnerAction",
  "CounterAction",
  "DelayAction",
  "GateAction",
  "CycleSignalAction",
  "RandomSignalAction",
  "UIArenaWaveAction",
];

const NUMERIC_TYPES = [
  "UInt8", "UInt16", "UInt32", "UInt64",
  "Int8", "Int16", "Int32", "Int64",
  "Float", "Double",
];

const TEXT_TYPES = ["String", "Enum", "Bitfield", "File", "Json"];

/** Plain-English names for the node types the pacing section shows. */
const BLOB_NAMES: Record<string, string> = {
  SpawnerScriptAction: "Spawn pacing",
  SunsetSpawnerFactoryScriptAction: "Bot setup",
  GameSpawnerAction: "Pickup spawner",
  CounterAction: "Counter",
  DelayAction: "Delay",
  GateAction: "Gate",
  CycleSignalAction: "Signal cycle",
  RandomSignalAction: "Random signal",
  UIArenaWaveAction: "Wave HUD",
};

/** Sections of the pacing tab, in the order they are shown. */
const BLOB_ORDER = [
  "SpawnerScriptAction",
  "SunsetSpawnerFactoryScriptAction",
  "GameSpawnerAction",
  "UIArenaWaveAction",
  "CounterAction",
  "DelayAction",
  "GateAction",
  "CycleSignalAction",
  "RandomSignalAction",
];

/**
 * Field names are long CamelCase identifiers with no break opportunities, so
 * a narrow column splits them mid-word. Zero-width spaces before each capital
 * let them wrap at word boundaries instead.
 */
function softWrap(name: string) {
  return name.replace(/(?<=[a-z0-9])(?=[A-Z])/g, "​");
}

/** Pull the couple of bot fields worth showing without expanding the card. */
function botSummary(blob: ArenaPrius): string[] {
  const json = parseTypedSafe(blob.json) as Record<string, unknown>;
  const dig = (path: string[]): unknown => {
    let cur: unknown = json;
    for (const seg of path) {
      const entry = (cur as Record<string, Typed> | undefined)?.[seg];
      if (!entry) return undefined;
      cur = entry.Value;
    }
    return cur;
  };
  const out: string[] = [];
  const health = dig(["BotBaseData", "Health"]);
  if (typeof health === "number") out.push(`${health} HP`);
  const pool = dig(["BotData", "AttackJobPool"]);
  if (typeof pool === "string") out.push(pool.replace(/^k/, ""));
  if (dig(["BotData", "OneHitDeath"]) === true) out.push("one-hit");
  return out;
}

/** Number of editable leaves in a parsed prius, for the collapsed summary. */
function countLeaves(parsed: Record<string, Typed>): number {
  let n = 0;
  for (const entry of Object.values(parsed)) {
    if (!isTyped(entry)) continue;
    if (entry.Type === "Struct" && !entry.ArrayKind && typeof entry.Value === "object" && entry.Value) {
      n += countLeaves(entry.Value as Record<string, Typed>);
    } else {
      n += 1;
    }
  }
  return n;
}

function friendlyBlobName(label: string) {
  const types = label.split(" / ");
  const named = types.map((t) => BLOB_NAMES[t] ?? t);
  return named.join(" / ");
}

const STYLE_ORDER = ["volume", "portal", "animclue", "static"];

const STYLE_LABELS: Record<string, string> = {
  volume: "Spawn volumes — enemy just appears",
  portal: "Rift portals — enemy needs a portal entry animation",
  animclue: "Anim clues — enemy plays a scripted entry",
  static: "Generic static volumes — also used for triggers and bounce pads",
};

interface ArenaPrius {
  id: number;
  label: string;
  owners: string[];
  json: string;
  size: number;
}

interface ArenaActorAsset {
  index: number;
  path: string;
  asset_id: string;
  instances: string[];
  is_enemy: boolean;
  prius_ids: number[];
}

interface ArenaAssetRef {
  index: number;
  path: string;
  asset_id: string;
  ext_hash: string;
}

interface ArenaSpawnBinding {
  var: number;
  kind: "actor" | "group" | "unresolved";
  id: string;
  label: string;
  asset: string;
  style: string;
  via: string;
}

interface ArenaSpawnTarget {
  id: string;
  kind: "actor" | "group";
  label: string;
  asset: string;
  count: number;
  style: string;
}

interface ArenaSpawner {
  node: number;
  template: string;
  num_spawns: number | null;
  num_spawns_var: number | null;
  max_simultaneous: number | null;
  prius_id: number | null;
  groups: string[];
  locations: ArenaSpawnBinding[];
}

interface ArenaWave {
  number: number;
  signals: string[];
  spawners: ArenaSpawner[];
  total: number;
}

interface ArenaZoneData {
  zone_name: string;
  action_count: number;
  actor_count: number;
  waves: ArenaWave[];
  node_type_counts: [string, number][];
  actor_groups: string[];
  script_priuses: ArenaPrius[];
  actor_priuses: ArenaPrius[];
  actor_assets: ArenaActorAsset[];
  model_names: string[];
  asset_refs: ArenaAssetRef[];
  spawn_targets: ArenaSpawnTarget[];
}

type Typed = { Type: string; ArrayKind?: string; Value: unknown };
type BlobKind = "script" | "actor";
type PatchMap = Record<string, Record<string, unknown>>;

/** Quote 16+ digit integers so 64-bit ids survive `JSON.parse` for display. */
function parseTypedSafe(text: string): Record<string, Typed> {
  try {
    return JSON.parse(text.replace(/("Value":\s*)(-?\d{16,})/g, '$1"$2"'));
  } catch {
    return {};
  }
}

function isTyped(v: unknown): v is Typed {
  return typeof v === "object" && v !== null && "Type" in v && "Value" in v;
}

export default function ArenaEditor() {
  const location = useLocation();
  const [zonePath, setZonePath] = useState("");
  const [outPath, setOutPath] = useState("");
  const [overwriteInput, setOverwriteInput] = useState(false);
  const [data, setData] = useState<ArenaZoneData | null>(null);
  const [tab, setTab] = useState<"waves" | "pacing" | "enemies" | "raw">("waves");
  const [showAllPacing, setShowAllPacing] = useState(false);
  const [pacingSearch, setPacingSearch] = useState("");
  const [showAllAssets, setShowAllAssets] = useState(false);

  const [patches, setPatches] = useState<PatchMap>({});
  const [rawEdits, setRawEdits] = useState<Record<string, string>>({});
  const [assetEdits, setAssetEdits] = useState<Record<number, string>>({});
  const [varEdits, setVarEdits] = useState<Record<number, number>>({});
  const [varIdEdits, setVarIdEdits] = useState<Record<number, string>>({});
  const [cloneRequests, setCloneRequests] = useState<
    { source: number; new_number: number }[]
  >([]);
  const [cloneCounts, setCloneCounts] = useState<Record<number, number>>({});
  const [rawSelected, setRawSelected] = useState<string | null>(null);
  const [rawSearch, setRawSearch] = useState("");
  const [rawError, setRawError] = useState("");

  const [sendToStager, setSendToStager] = useState<string | null>(null);
  const [assetPath, setAssetPath] = useState<string | null>(null);
  const [log, setLog] = useState<LogEntry[]>([]);
  const [running, setRunning] = useState(false);

  useEffect(() => {
    if (location.pathname !== "/tools/arena-editor") return;
    const params = new URLSearchParams(location.search);
    const s = location.state as { filePath?: string; assetPath?: string } | null;
    const filePath = s?.filePath ?? params.get("filePath") ?? undefined;
    const incoming = s?.assetPath ?? params.get("assetPath") ?? undefined;
    if (filePath) setZonePath(filePath);
    if (incoming) setAssetPath(incoming);
  }, [location.pathname, location.state, location.search]);

  function pushLog(type: LogEntry["type"], message: string) {
    setLog((prev) => [...prev, { type, message, ts: Date.now() }]);
  }

  const resetEdits = useCallback(() => {
    setPatches({});
    setRawEdits({});
    setAssetEdits({});
    setVarEdits({});
    setVarIdEdits({});
    setCloneRequests([]);
    setRawSelected(null);
    setRawError("");
  }, []);

  const handleZonePathChange = useCallback(
    (p: string) => {
      setZonePath(p);
      setData(null);
      resetEdits();
      setLog([]);
    },
    [resetEdits],
  );

  async function loadZone() {
    if (!zonePath) {
      pushLog("error", "Select a gameplay .zone file first.");
      return;
    }
    setRunning(true);
    setData(null);
    resetEdits();
    setLog([]);
    try {
      pushLog("info", `Reading ${zonePath} …`);
      const result = await invoke<ArenaZoneData>("read_arena_zone", { zonePath });
      setData(result);
      pushLog(
        "success",
        `${result.action_count} script nodes, ${result.actor_count} actors, ` +
          `${result.waves.length} wave(s), ${result.script_priuses.length + result.actor_priuses.length} tunable blobs.`,
      );
      if (result.waves.length === 0) {
        pushLog("warning", "No wave signals found — this may not be an arena challenge zone.");
      }
    } catch (e) {
      pushLog("error", String(e));
    } finally {
      setRunning(false);
    }
  }

  const blobKey = (kind: BlobKind, id: number) => `${kind}:${id}`;

  function setPatch(kind: BlobKind, id: number, path: string[], value: unknown) {
    const key = blobKey(kind, id);
    setPatches((prev) => ({
      ...prev,
      [key]: { ...(prev[key] ?? {}), [JSON.stringify(path)]: value },
    }));
  }

  function clearBlobEdits(kind: BlobKind, id: number) {
    const key = blobKey(kind, id);
    setPatches((prev) => {
      const next = { ...prev };
      delete next[key];
      return next;
    });
    setRawEdits((prev) => {
      const next = { ...prev };
      delete next[key];
      return next;
    });
  }

  function patchedValue(kind: BlobKind, id: number, path: string[], fallback: unknown) {
    const entry = patches[blobKey(kind, id)];
    const hit = entry?.[JSON.stringify(path)];
    return hit === undefined ? fallback : hit;
  }

  const dirtyCount =
    Object.keys(patches).length +
    Object.keys(rawEdits).length +
    Object.keys(assetEdits).length +
    Object.keys(varEdits).length +
    Object.keys(varIdEdits).length +
    cloneRequests.length;

  function buildPayload() {
    const scriptPatches: Record<number, { path: string[]; value: unknown }[]> = {};
    const actorPatches: Record<number, { path: string[]; value: unknown }[]> = {};
    for (const [key, fields] of Object.entries(patches)) {
      const [kind, idText] = key.split(":");
      const id = Number(idText);
      const list = Object.entries(fields).map(([p, value]) => ({
        path: JSON.parse(p) as string[],
        value,
      }));
      if (kind === "script") scriptPatches[id] = list;
      else actorPatches[id] = list;
    }

    const scriptJson: Record<number, string> = {};
    const actorJson: Record<number, string> = {};
    for (const [key, text] of Object.entries(rawEdits)) {
      const [kind, idText] = key.split(":");
      const id = Number(idText);
      if (kind === "script") scriptJson[id] = text;
      else actorJson[id] = text;
    }

    return JSON.stringify({
      script_prius_patches: scriptPatches,
      actor_prius_patches: actorPatches,
      script_prius_json: scriptJson,
      actor_prius_json: actorJson,
      actor_assets: assetEdits,
      script_vars: varEdits,
      script_var_ids: varIdEdits,
      clone_waves: cloneRequests,
    });
  }

  async function saveZone() {
    if (!zonePath || !data) return;
    if (rawError) {
      pushLog("error", "Fix the JSON error before saving.");
      return;
    }
    setRunning(true);
    try {
      pushLog("info", `Writing ${dirtyCount} edit(s) …`);
      const result = await invoke<string>("write_arena_zone", {
        zonePath,
        editsJson: buildPayload(),
        outPath: overwriteInput ? zonePath : outPath || null,
      });
      pushLog("success", `Saved → ${result}`);
    } catch (e) {
      pushLog("error", String(e));
    } finally {
      setRunning(false);
    }
  }

  async function verifyRoundtrip() {
    if (!zonePath) return;
    setRunning(true);
    try {
      const result = await invoke<string>("verify_arena_zone_roundtrip", { zonePath });
      pushLog(result.startsWith("byte-identical") ? "success" : "warning", result);
    } catch (e) {
      pushLog("error", String(e));
    } finally {
      setRunning(false);
    }
  }

  // -------------------------------------------------------------------------
  // field rendering
  // -------------------------------------------------------------------------

  /**
   * Nested structs render as a heading plus flat rows rather than indented
   * blocks — the labels are long and indentation left no room for them.
   */
  function renderField(
    kind: BlobKind,
    id: number,
    name: string,
    entry: Typed,
    path: string[],
  ): ReactElement[] {
    const full = [...path, name];
    const keyed = full.join(".");
    const one = (el: ReactElement) => [el];

    if (entry.Type === "Struct" && !entry.ArrayKind && typeof entry.Value === "object" && entry.Value) {
      const inner = entry.Value as Record<string, Typed>;
      const children = Object.entries(inner).flatMap(([k, v]) =>
        isTyped(v) ? renderField(kind, id, k, v, full) : [],
      );
      if (children.length === 0) return [];
      return [
        <span className={styles.fieldGroupLabel} key={`${keyed}-h`}>
          {softWrap(full.join(" › "))}
        </span>,
        ...children,
      ];
    }

    if (entry.ArrayKind || Array.isArray(entry.Value)) {
      return one(
        <div className={styles.fieldRow} key={keyed}>
          <span className={styles.fieldLabel} title={name}>{softWrap(name)}</span>
          <span className={styles.readOnly}>{JSON.stringify(entry.Value)}</span>
        </div>
      );
    }

    if (entry.Type === "Identifier" || entry.Type === "Asset") {
      return one(
        <div className={styles.fieldRow} key={keyed}>
          <span className={styles.fieldLabel} title={name}>{softWrap(name)}</span>
          <span className={styles.readOnly}>{String(entry.Value)}</span>
        </div>
      );
    }

    const current = patchedValue(kind, id, full, entry.Value);

    if (entry.Type === "Bool") {
      return one(
        <div className={styles.fieldRow} key={keyed}>
          <span className={styles.fieldLabel} title={name}>{softWrap(name)}</span>
          <input
            type="checkbox"
            checked={Boolean(current)}
            onChange={(e) => setPatch(kind, id, full, e.target.checked)}
          />
        </div>
      );
    }

    if (NUMERIC_TYPES.includes(entry.Type)) {
      const isFloat = entry.Type === "Float" || entry.Type === "Double";
      return one(
        <div className={styles.fieldRow} key={keyed}>
          <span className={styles.fieldLabel} title={name}>{softWrap(name)}</span>
          <input
            className={styles.valueInput}
            type="number"
            step={isFloat ? "any" : 1}
            value={String(current ?? "")}
            onChange={(e) => {
              const n = e.target.value === "" ? 0 : Number(e.target.value);
              setPatch(kind, id, full, isFloat ? n : Math.trunc(n));
            }}
          />
        </div>
      );
    }

    if (TEXT_TYPES.includes(entry.Type)) {
      return one(
        <div className={styles.fieldRow} key={keyed}>
          <span className={styles.fieldLabel} title={name}>{softWrap(name)}</span>
          <input
            className={styles.valueInput}
            value={String(current ?? "")}
            onChange={(e) => setPatch(kind, id, full, e.target.value)}
          />
        </div>
      );
    }

    return one(
      <div className={styles.fieldRow} key={keyed}>
        <span className={styles.fieldLabel} title={name}>{softWrap(name)}</span>
        <span className={styles.readOnly}>{String(entry.Value)}</span>
      </div>
    );
  }

  function renderBlobCard(kind: BlobKind, blob: ArenaPrius) {
    const key = blobKey(kind, blob.id);
    const dirty = Boolean(patches[key] || rawEdits[key]);
    const parsed = parseTypedSafe(rawEdits[key] ?? blob.json);
    const fields = Object.entries(parsed).flatMap(([k, v]) =>
      isTyped(v) ? renderField(kind, blob.id, k, v, []) : [],
    );
    const waves = (data?.waves ?? [])
      .filter((w) => w.spawners.some((s) => s.prius_id === blob.id))
      .map((w) => w.number);

    return (
      <div
        className={`${styles.blobCard} ${dirty ? styles.blobCardDirty : ""}`}
        key={key}
      >
        <div className={styles.blobHead}>
          <span className={styles.blobTitle}>{friendlyBlobName(blob.label)}</span>
          {waves.length > 0 && (
            <span className={styles.waveTag}>
              wave{waves.length > 1 ? "s" : ""} {waves.join(", ")}
            </span>
          )}
          <span className={styles.spacer} />
          <span
            className={styles.blobMeta}
            title={`${blob.label}  ·  nodes ${blob.owners.join(", ")}`}
          >
            ×{blob.owners.length}
          </span>
        </div>
        <div className={styles.fieldTable}>
          {fields.length > 0 ? fields : <p className={styles.emptyText}>No overridden fields.</p>}
        </div>
        {dirty && (
          <button className={styles.secondaryBtn} onClick={() => clearBlobEdits(kind, blob.id)}>
            Revert
          </button>
        )}
      </div>
    );
  }

  /**
   * Queue `count` duplications. Every copy is taken from the original wave —
   * the backend chains them onto each other so they still run in order.
   */
  function queueClone(source: number, count: number) {
    setCloneRequests((prev) => {
      let highest = Math.max(
        ...(data?.waves.map((w) => w.number) ?? [0]),
        ...prev.map((c) => c.new_number),
      );
      const added = [];
      for (let i = 0; i < count; i++) {
        highest += 1;
        added.push({ source, new_number: highest });
      }
      return [...prev, ...added];
    });
  }

  /** Wave size with any pending `NumSpawns` edits folded in. */
  function waveTotal(w: ArenaWave) {
    return w.spawners.reduce((sum, s) => {
      const edited = s.num_spawns_var !== null ? varEdits[s.num_spawns_var] : undefined;
      return sum + (edited ?? s.num_spawns ?? 0);
    }, 0);
  }

  // -------------------------------------------------------------------------
  // tabs
  // -------------------------------------------------------------------------

  /** Pacing blobs bucketed by node type, most useful sections first. */
  const pacingGroups = useMemo(() => {
    if (!data) return [];
    const q = pacingSearch.trim().toLowerCase();
    const kept = data.script_priuses.filter((b) => {
      const types = b.label.split(" / ");
      if (!showAllPacing && !types.some((t) => PACING_TYPES.includes(t))) return false;
      if (!q) return true;
      return (
        b.label.toLowerCase().includes(q) ||
        friendlyBlobName(b.label).toLowerCase().includes(q) ||
        b.json.toLowerCase().includes(q)
      );
    });
    const buckets = new Map<string, ArenaPrius[]>();
    for (const b of kept) {
      const type = b.label.split(" / ")[0] ?? "other";
      const list = buckets.get(type);
      if (list) list.push(b);
      else buckets.set(type, [b]);
    }
    return [...buckets.entries()].sort((a, b) => {
      const ra = BLOB_ORDER.indexOf(a[0]);
      const rb = BLOB_ORDER.indexOf(b[0]);
      return (ra < 0 ? 99 : ra) - (rb < 0 ? 99 : rb) || a[0].localeCompare(b[0]);
    });
  }, [data, showAllPacing, pacingSearch]);

  const visibleAssets = useMemo(() => {
    if (!data) return [];
    return data.actor_assets.filter((a) => showAllAssets || a.is_enemy);
  }, [data, showAllAssets]);

  const rawBlobs = useMemo(() => {
    if (!data) return [];
    const all = [
      ...data.script_priuses.map((b) => ({ kind: "script" as BlobKind, blob: b })),
      ...data.actor_priuses.map((b) => ({ kind: "actor" as BlobKind, blob: b })),
    ];
    const q = rawSearch.trim().toLowerCase();
    if (!q) return all;
    return all.filter(
      ({ blob }) =>
        blob.label.toLowerCase().includes(q) || blob.json.toLowerCase().includes(q),
    );
  }, [data, rawSearch]);

  const selectedRaw = useMemo(() => {
    if (!rawSelected || !data) return null;
    const [kind, idText] = rawSelected.split(":");
    const list = kind === "script" ? data.script_priuses : data.actor_priuses;
    const blob = list.find((b) => b.id === Number(idText));
    return blob ? { kind: kind as BlobKind, blob } : null;
  }, [rawSelected, data]);

  return (
    <div className={styles.page}>
      <h2 className={styles.title}>Arena Editor</h2>
      <p className={styles.subtitle}>
        Retune arena challenge waves and swap the enemies they spawn
      </p>

      <div className={styles.panel}>
        <FilePickerInput
          label="Gameplay zone"
          value={zonePath}
          onChange={handleZonePathChange}
          mode="open"
          filters={ZONE_FILTER}
          placeholder="levels\i29\instance\zurkons\zurkons_visit1\…_arena_challenge_a1.zone"
        />
        <div className={styles.loadRow}>
          <button className={styles.runBtn} onClick={loadZone} disabled={running || !zonePath}>
            {running ? "Loading…" : "Load Zone"}
          </button>
          <button
            className={styles.secondaryBtn}
            onClick={verifyRoundtrip}
            disabled={running || !zonePath}
            title="Re-emit the zone with no edits and check it is byte-identical"
          >
            Verify round-trip
          </button>
        </div>
      </div>

      {data && (
        <>
          <div className={styles.summaryBar}>
            <span className={styles.summaryName} title={zonePath}>
              {data.zone_name}
            </span>
            <span className={styles.summaryStats}>
              {[
                [data.waves.length + cloneRequests.length, "waves"],
                [data.waves.reduce((n, w) => n + waveTotal(w), 0), "wave enemies"],
                [data.actor_assets.filter((a) => a.is_enemy).length, "enemy assets"],
                [data.action_count, "script nodes"],
                [data.actor_count, "actors"],
              ].map(([value, label]) => (
                <span className={styles.stat} key={label}>
                  <strong>{value}</strong> {label}
                </span>
              ))}
            </span>
          </div>

          <div className={styles.tabs}>
            <button
              className={`${styles.tab} ${tab === "waves" ? styles.tabActive : ""}`}
              onClick={() => setTab("waves")}
            >
              Waves
            </button>
            <button
              className={`${styles.tab} ${tab === "pacing" ? styles.tabActive : ""}`}
              onClick={() => setTab("pacing")}
            >
              Pacing
            </button>
            <button
              className={`${styles.tab} ${tab === "enemies" ? styles.tabActive : ""}`}
              onClick={() => setTab("enemies")}
            >
              Enemies
            </button>
            <button
              className={`${styles.tab} ${tab === "raw" ? styles.tabActive : ""}`}
              onClick={() => setTab("raw")}
            >
              All Priuses
            </button>
          </div>

          <div className={styles.tabBody}>
            {tab === "waves" && (
              <>
                <details className={styles.explainer}>
                  <summary>
                    Each wave runs one spawner per portal — set how many enemies each sends and
                    where they arrive from.
                  </summary>
                  <p>
                    <strong>Count</strong> is the spawner&apos;s <code>NumSpawns</code>, a script
                    variable rather than a prius field, so editing it rewrites the variable
                    directly. <strong>Max</strong> is the concurrency cap, not a total.
                  </p>
                  <p>
                    <strong>Spawns from</strong> is how the enemies arrive. A <em>portal</em>{" "}
                    makes the bot leap out of a rift tear and needs an entry animation it may not
                    have; a <em>volume</em> just places it. Switching to a volume is the fix when
                    a swapped-in enemy spawns but never enters the arena.
                  </p>
                  <p>
                    <strong>Duplicate</strong> copies the whole wave and runs the copies straight
                    after it. Reload the zone afterwards to tune them.
                  </p>
                </details>
                <div className={styles.waveList}>
                  {data.waves.map((w) => (
                    <section className={styles.waveCard} key={w.number}>
                      <header className={styles.waveHead}>
                        <h4>Wave {w.number}</h4>
                        <span className={styles.waveTotal}>
                          {waveTotal(w)} enem{waveTotal(w) === 1 ? "y" : "ies"}
                        </span>
                        <span className={styles.spacer} />
                        <span className={styles.dupLabel}>copies</span>
                        <input
                          className={styles.cloneCount}
                          type="number"
                          min={1}
                          max={20}
                          value={cloneCounts[w.number] ?? 1}
                          onChange={(e) =>
                            setCloneCounts((prev) => ({
                              ...prev,
                              [w.number]: Math.max(
                                1,
                                Math.min(20, Number(e.target.value) || 1),
                              ),
                            }))
                          }
                          title="How many copies to make"
                        />
                        <button
                          className={styles.cloneBtn}
                          onClick={() => queueClone(w.number, cloneCounts[w.number] ?? 1)}
                          title="Duplicate this wave and run the copies straight after it"
                        >
                          Duplicate
                        </button>
                      </header>

                      {w.spawners.length > 0 && (
                        <div className={styles.spawnHead}>
                          <span>Enemy</span>
                          <span className={styles.alignRight}>Count</span>
                          <span className={styles.alignRight}>Max</span>
                          <span>Spawns from</span>
                        </div>
                      )}
                      {w.spawners.map((s) => {
                        const edited = s.num_spawns_var !== null && varEdits[s.num_spawns_var] !== undefined;
                        return (
                          <div className={styles.spawnerRow} key={s.node}>
                            <span
                              className={styles.templateName}
                              title={`script node ${s.node}`}
                            >
                              {s.template || `node ${s.node}`}
                            </span>
                            {s.num_spawns_var !== null ? (
                              <input
                                className={`${styles.numInput} ${edited ? styles.numInputDirty : ""}`}
                                type="number"
                                min={0}
                                step={1}
                                value={String(
                                  varEdits[s.num_spawns_var] ?? s.num_spawns ?? 0,
                                )}
                                onChange={(e) =>
                                  setVarEdits((prev) => ({
                                    ...prev,
                                    [s.num_spawns_var as number]:
                                      e.target.value === "" ? 0 : Number(e.target.value),
                                  }))
                                }
                              />
                            ) : (
                              <span className={styles.muted}>—</span>
                            )}
                            <span className={`${styles.muted} ${styles.alignRight}`}>
                              {s.max_simultaneous ?? "?"}
                            </span>
                            <div className={styles.fromCell}>
                            {s.locations.length === 0 && (
                              <span className={styles.muted}>—</span>
                            )}
                            {s.locations.map((b, bi) => (
                            <div className={styles.fromRow} key={`${s.node}-${bi}`}>
                              {b.kind === "unresolved" ? (
                                <span className={styles.muted} title={b.via}>
                                  {b.label}
                                </span>
                              ) : (
                                <select
                                  className={`${styles.fromSelect} ${
                                    varIdEdits[b.var] !== undefined ? styles.fromSelectDirty : ""
                                  } ${
                                    b.style === "static" || b.style === "other"
                                      ? styles.fromSelectWarn
                                      : ""
                                  }`}
                                  title={
                                    b.style === "static" || b.style === "other"
                                      ? `${b.asset || "unknown asset"} is not a purpose-built spawn point — var ${b.var}`
                                      : `${b.asset} · var ${b.var} · ${b.via}`
                                  }
                                  value={varIdEdits[b.var] ?? b.id}
                                  onChange={(e) =>
                                    setVarIdEdits((prev) => ({ ...prev, [b.var]: e.target.value }))
                                  }
                                >
                                  {!data.spawn_targets.some((t) => t.id === b.id) && (
                                    <option value={b.id}>{b.label} (current)</option>
                                  )}
                                  {STYLE_ORDER.map((style) => {
                                    // A binding read by an ActorPick needs a
                                    // group; a direct one needs a single actor.
                                    const opts = data.spawn_targets.filter(
                                      (t) => t.style === style && t.kind === b.kind,
                                    );
                                    if (opts.length === 0) return null;
                                    return (
                                      <optgroup key={style} label={STYLE_LABELS[style]}>
                                        {opts.map((t) => (
                                          <option key={t.id} value={t.id}>
                                            {t.kind === "group"
                                              ? `${t.label} [${t.count}]`
                                              : t.label}
                                          </option>
                                        ))}
                                      </optgroup>
                                    );
                                  })}
                                </select>
                              )}
                            </div>
                            ))}
                            </div>
                          </div>
                        );
                      })}
                      {w.spawners.length === 0 && (
                        <p className={styles.emptyText}>No spawners bound to this wave.</p>
                      )}
                      {w.signals.length > 0 && (
                        <details className={styles.signals}>
                          <summary>{w.signals.length} signals</summary>
                          <div className={styles.chips}>
                            {w.signals.map((s) => (
                              <span className={styles.chip} key={s}>
                                {s}
                              </span>
                            ))}
                          </div>
                        </details>
                      )}
                    </section>
                  ))}
                  {data.waves.length === 0 && (
                    <p className={styles.emptyText}>No waves found in this zone.</p>
                  )}
                  {cloneRequests.map((c, i) => (
                    <div className={styles.waveCard} key={`clone-${i}`}>
                      <div className={styles.waveHead}>
                        <h4>Wave {c.new_number}</h4>
                        <span className={styles.waveTotal}>copy of wave {c.source}</span>
                        <span className={styles.spacer} />
                        <button
                          className={styles.cloneBtn}
                          onClick={() =>
                            setCloneRequests((prev) => prev.filter((_, k) => k !== i))
                          }
                        >
                          Remove
                        </button>
                      </div>
                      <p className={styles.emptyText}>
                        Created on save — reload the zone afterwards to tune it.
                      </p>
                    </div>
                  ))}
                </div>
              </>
            )}

            {tab === "pacing" && (
              <>
                <div className={styles.sectionHead}>
                  <h3>Spawn pacing</h3>
                  <input
                    className={styles.pacingSearch}
                    placeholder="Filter fields…"
                    value={pacingSearch}
                    onChange={(e) => setPacingSearch(e.target.value)}
                  />
                  <label className={styles.overwriteToggle}>
                    <input
                      type="checkbox"
                      checked={showAllPacing}
                      onChange={(e) => setShowAllPacing(e.target.checked)}
                    />
                    <span>Show every script blob</span>
                  </label>
                </div>
                <p className={styles.helperText}>
                  Each card holds one node&apos;s overrides. <code>×n</code> means n nodes share
                  it, so an edit applies to all of them.
                </p>
                {pacingGroups.map(([type, blobs]) => (
                  <div className={styles.blobGroup} key={type}>
                    <h4 className={styles.blobGroupTitle}>
                      <span className={styles.blobGroupCount}>{blobs.length}</span>
                      {BLOB_NAMES[type] ?? type}
                      <span className={styles.blobGroupType}>{type}</span>
                    </h4>
                    <div className={styles.blobGrid}>
                      {blobs.map((b) => renderBlobCard("script", b))}
                    </div>
                  </div>
                ))}
                {pacingGroups.length === 0 && (
                  <p className={styles.emptyText}>Nothing matches that filter.</p>
                )}
              </>
            )}

            {tab === "enemies" && (
              <>
                <div className={styles.sectionHead}>
                  <h3>Actor assets</h3>
                  <label className={styles.overwriteToggle}>
                    <input
                      type="checkbox"
                      checked={showAllAssets}
                      onChange={(e) => setShowAllAssets(e.target.checked)}
                    />
                    <span>Show all {data.actor_assets.length} assets</span>
                  </label>
                </div>
                <p className={styles.helperText}>
                  Editing a path re-hashes its asset id on save. The zone still preloads the
                  original model list, so pick a replacement whose model ships with this level or
                  add the new one through the Stager.
                </p>
                {visibleAssets.map((asset) => {
                  const priuses = asset.prius_ids
                    .map((id) => data.actor_priuses.find((b) => b.id === id))
                    .filter((b): b is ArenaPrius => Boolean(b));
                  const edited = assetEdits[asset.index] !== undefined;
                  const path = assetEdits[asset.index] ?? asset.path;
                  const waves = (data.waves ?? [])
                    .filter((w) =>
                      w.spawners.some((s) => asset.instances.includes(s.template)),
                    )
                    .map((w) => w.number);
                  const stats = priuses.flatMap(botSummary);
                  const fieldCount = priuses.reduce(
                    (n, b) => n + countLeaves(parseTypedSafe(b.json)),
                    0,
                  );
                  return (
                    <div className={styles.enemyCard} key={asset.index}>
                      <div className={styles.enemyHead}>
                        <span className={styles.enemyName}>
                          {asset.instances[0] ?? `asset ${asset.index}`}
                        </span>
                        {asset.instances.length > 1 && (
                          <span className={styles.blobMeta}>
                            ×{asset.instances.length} instances
                          </span>
                        )}
                        {waves.length > 0 && (
                          <span className={styles.waveTag}>
                            wave{waves.length > 1 ? "s" : ""} {waves.join(", ")}
                          </span>
                        )}
                        {stats.map((s) => (
                          <span className={styles.statTag} key={s}>
                            {s}
                          </span>
                        ))}
                        <span className={styles.spacer} />
                        <span className={styles.blobMeta} title="asset id">
                          {edited ? "re-hashed on save" : asset.asset_id}
                        </span>
                      </div>

                      <div className={styles.pathRow}>
                        <input
                          className={`${styles.pathInput} ${edited ? styles.pathInputDirty : ""}`}
                          value={path}
                          spellCheck={false}
                          title={path}
                          onChange={(e) =>
                            setAssetEdits((prev) => ({ ...prev, [asset.index]: e.target.value }))
                          }
                        />
                        {edited && (
                          <button
                            className={styles.cloneBtn}
                            onClick={() =>
                              setAssetEdits((prev) => {
                                const next = { ...prev };
                                delete next[asset.index];
                                return next;
                              })
                            }
                          >
                            Revert
                          </button>
                        )}
                      </div>

                      {priuses.length > 0 && (
                        <details className={styles.statsBlock}>
                          <summary>
                            Stats &amp; configs — {priuses.map((b) => b.label).join(", ")} (
                            {fieldCount} fields)
                          </summary>
                          <div className={styles.blobGrid}>
                            {priuses.map((b) => renderBlobCard("actor", b))}
                          </div>
                        </details>
                      )}
                    </div>
                  );
                })}
                {visibleAssets.length === 0 && (
                  <p className={styles.emptyText}>No enemy actors detected in this zone.</p>
                )}
              </>
            )}

            {tab === "raw" && (
              <div className={styles.rawLayout}>
                <div className={styles.rawList}>
                  <input
                    className={styles.rawSearch}
                    placeholder="Filter…"
                    value={rawSearch}
                    onChange={(e) => setRawSearch(e.target.value)}
                  />
                  {rawBlobs.map(({ kind, blob }) => {
                    const key = blobKey(kind, blob.id);
                    const dirty = Boolean(patches[key] || rawEdits[key]);
                    return (
                      <button
                        key={key}
                        className={`${styles.rawItem} ${
                          rawSelected === key ? styles.rawItemActive : ""
                        } ${dirty ? styles.rawItemDirty : ""}`}
                        onClick={() => {
                          setRawSelected(key);
                          setRawError("");
                        }}
                      >
                        {kind === "actor" ? "◆ " : ""}
                        {blob.label || `blob ${blob.id}`}
                      </button>
                    );
                  })}
                </div>
                <div className={styles.rawEditor}>
                  {selectedRaw ? (
                    <CodeMirror
                      value={
                        rawEdits[blobKey(selectedRaw.kind, selectedRaw.blob.id)] ??
                        selectedRaw.blob.json
                      }
                      height="100%"
                      theme={vscodeDark}
                      extensions={[jsonLang()]}
                      onChange={(text) => {
                        setRawEdits((prev) => ({
                          ...prev,
                          [blobKey(selectedRaw.kind, selectedRaw.blob.id)]: text,
                        }));
                        try {
                          JSON.parse(text);
                          setRawError("");
                        } catch (e) {
                          setRawError(String(e));
                        }
                      }}
                      basicSetup={{ lineNumbers: true, foldGutter: true }}
                    />
                  ) : (
                    <p className={styles.emptyText}>Select a blob to edit its raw DDL JSON.</p>
                  )}
                </div>
                {rawError && <span className={styles.errorMsg}>{rawError}</span>}
              </div>
            )}
          </div>

          <div className={styles.actions}>
            <FilePickerInput
              label="Output file (optional)"
              value={outPath}
              onChange={setOutPath}
              mode="save"
              filters={ZONE_FILTER}
              placeholder="Leave blank — saves as _edited.zone"
            />
            <div className={styles.actionRow}>
              <button
                className={styles.secondaryBtn}
                onClick={() =>
                  setSendToStager(overwriteInput ? zonePath : outPath || zonePath)
                }
                disabled={running || !!rawError}
                title="Send output to a Stager project"
              >
                Send to Stager
              </button>
              <button
                className={styles.runBtn}
                onClick={saveZone}
                disabled={running || !!rawError || dirtyCount === 0}
              >
                {running ? "Saving…" : "Save Zone"}
              </button>
              <label className={styles.overwriteToggle}>
                <input
                  type="checkbox"
                  checked={overwriteInput}
                  onChange={(e) => setOverwriteInput(e.target.checked)}
                />
                <span>Overwrite input file</span>
              </label>
              {dirtyCount > 0 && (
                <span className={styles.dirtyCount}>{dirtyCount} pending edit(s)</span>
              )}
            </div>
          </div>
        </>
      )}

      <StatusLog entries={log} />

      {sendToStager && (
        <SendToStagerModal
          sourceFile={sendToStager}
          defaultTargetPath={deriveStagerTarget(sendToStager, assetPath)}
          onClose={() => setSendToStager(null)}
          onSent={(proj) => {
            setSendToStager(null);
            pushLog("success", `Sent to project "${proj}"`);
          }}
        />
      )}
    </div>
  );
}
