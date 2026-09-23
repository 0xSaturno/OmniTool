import {
  useCallback,
  useEffect,
  useMemo,
  useReducer,
  useRef,
  useState,
} from "react";
import { useLocation } from "react-router-dom";
import { LuLayers3, LuSave, LuSettings } from "react-icons/lu";
import { useSettings } from "../../contexts/SettingsContext";
import { useProjects } from "../../contexts/ProjectsContext";
import StatusLog, { type LogEntry } from "../../components/shared/StatusLog";
import Viewport from "./Viewport";
import { KIND_MODEL, zoneGeometryKey, type SceneObjectDto, type ZoneScene } from "./types";
import * as api from "./api";
import type { LevelRegion, LevelZoneStats, ZoneEdit, ZoneEntry } from "./api";
import ZoneBrowser, { type BulkLoadProgress } from "./panels/ZoneBrowser";
import {
  analyseRegionMix,
  relationSentence,
  resolveRegionKey,
  spreadStep,
  type RegionLoadSummary,
  type RegionMixReport,
} from "./panels/RegionIndex";
import SceneTree from "./panels/SceneTree";
import Inspector, { type TransformValue } from "./panels/Inspector";
import Toolbar, { type ToolMode } from "./panels/Toolbar";
import StatusBar from "./panels/StatusBar";
import styles from "./LevelEditor.module.css";

/**
 * Several zones of one tile are loaded side by side (a tile's `_art` holds the
 * geometry, its `_gp` the gameplay actors), but a scene object is only unique
 * by `offset` *within* its own zone. Each loaded zone therefore gets a slot and
 * every object is presented to the viewport at `slot * OFFSET_STRIDE + offset`.
 * The stride is far above any zone asset size, and slot 0 is a no-op so the
 * single-zone case keeps the real offsets.
 */
const OFFSET_STRIDE = 1 << 27;

/**
 * "Load all" is happy to queue a whole planet (megalopolis alone lists 2,191
 * zones), so anything past this many zones asks first.
 */
const BULK_CONFIRM_THRESHOLD = 25;

/**
 * Zones are loaded one at a time so the UI keeps painting, but they are pushed
 * into `loaded` in small batches — every commit rebuilds the composite scene
 * and the whole viewport, which is far too expensive to do 400 times.
 */
const BULK_FLUSH_EVERY = 8;

/**
 * Ceiling for "select all". The renderer recolours every selected instance and
 * the gizmo re-derives its centroid from the whole selection, so an unbounded
 * select-all on a full planet (~180,000 instances) would stall the frame. The
 * cap is reported in the status log whenever it bites.
 */
const SELECT_ALL_LIMIT = 25000;

interface LoadedZone {
  path: string;
  slot: number;
  scene: ZoneScene;
}

/**
 * Artificial X step between regions while "Lay regions out side by side" is on.
 *
 * The level format carries NO per-region placement transform — proven in
 * `src-tauri/src/bin/instance_placement.rs` and documented on
 * `panels/RegionIndex.tsx` — so any layout the tool draws for two regions at
 * once is the tool's invention. This one is deliberately a round number of
 * kilometres, is printed on the button, and disables transform editing while
 * it is on, so nobody can mistake it for something read out of the game.
 */
const SPREAD_MIN_METRES = 1000;

const editKey = (zonePath: string, offset: number) => `${zonePath}#${offset}`;

/* ------------------------------------------------------------------ */
/* Edit + undo/redo state                                              */
/* ------------------------------------------------------------------ */

interface HistoryEntry {
  key: string;
  from: TransformValue;
  to: TransformValue;
  original: TransformValue;
}

interface EditState {
  edits: Map<string, TransformValue>;
  history: HistoryEntry[];
  index: number;
}

type EditAction =
  | { type: "edit"; key: string; original: TransformValue; to: TransformValue }
  | { type: "revert"; key: string; original: TransformValue }
  | { type: "undo" }
  | { type: "redo" }
  | { type: "reset" };

function sameTransform(a: TransformValue, b: TransformValue) {
  if (a === b) return true;
  for (let i = 0; i < 16; i++) if (a.matrix[i] !== b.matrix[i]) return false;
  for (let i = 0; i < 3; i++) if (a.scale[i] !== b.scale[i]) return false;
  return true;
}

function applyValue(
  edits: Map<string, TransformValue>,
  key: string,
  value: TransformValue,
  original: TransformValue,
) {
  const next = new Map(edits);
  if (sameTransform(value, original)) next.delete(key);
  else next.set(key, value);
  return next;
}

function editReducer(state: EditState, action: EditAction): EditState {
  switch (action.type) {
    case "edit": {
      const from = state.edits.get(action.key) ?? action.original;
      if (sameTransform(from, action.to)) return state;
      const history = state.history.slice(0, state.index);
      history.push({ key: action.key, from, to: action.to, original: action.original });
      return {
        edits: applyValue(state.edits, action.key, action.to, action.original),
        history,
        index: history.length,
      };
    }
    case "revert": {
      const from = state.edits.get(action.key);
      if (!from) return state;
      const history = state.history.slice(0, state.index);
      history.push({
        key: action.key,
        from,
        to: action.original,
        original: action.original,
      });
      return {
        edits: applyValue(state.edits, action.key, action.original, action.original),
        history,
        index: history.length,
      };
    }
    case "undo": {
      if (state.index === 0) return state;
      const entry = state.history[state.index - 1];
      return {
        edits: applyValue(state.edits, entry.key, entry.from, entry.original),
        history: state.history,
        index: state.index - 1,
      };
    }
    case "redo": {
      if (state.index >= state.history.length) return state;
      const entry = state.history[state.index];
      return {
        edits: applyValue(state.edits, entry.key, entry.to, entry.original),
        history: state.history,
        index: state.index + 1,
      };
    }
    case "reset":
      return { edits: new Map(), history: [], index: 0 };
  }
}

const EMPTY_EDITS: EditState = { edits: new Map(), history: [], index: 0 };

/* ------------------------------------------------------------------ */

export default function LevelEditor() {
  // Every tool stays mounted behind `display: none`, so the editor must not
  // touch the archives until the user actually opens this route.
  const location = useLocation();
  const active = location.pathname === "/tools/level-editor";
  const { settings, setSettingsOpen } = useSettings();
  const gameDir = settings.archivesDir;
  const { projects, selectedProject, setSelectedProject, createProject } = useProjects();

  const [log, setLog] = useState<LogEntry[]>([]);
  const pushLog = useCallback((type: LogEntry["type"], message: string) => {
    setLog((prev) => [...prev.slice(-200), { type, message, ts: Date.now() }]);
  }, []);

  // ---- zone index ---------------------------------------------------------
  const [regions, setRegions] = useState<LevelRegion[]>([]);
  const [stats, setStats] = useState<LevelZoneStats | null>(null);
  const [activeRegion, setActiveRegion] = useState<string | null>(null);
  const [zones, setZones] = useState<ZoneEntry[]>([]);
  const [loadingRegions, setLoadingRegions] = useState(false);
  const [loadingZones, setLoadingZones] = useState(false);
  /**
   * Zone lists already fetched, by region key. The browser's one find box
   * searches regions across every group instantly, but a zone name can only be
   * matched inside a list we hold — so every region the user visits stays
   * searchable, and the panel offers to fetch the rest on request instead of
   * silently matching nothing.
   */
  const [zoneCache, setZoneCache] = useState<Map<string, ZoneEntry[]>>(() => new Map());
  /** Display-only side-by-side layout; see the note on SPREAD_NOTE below. */
  const [spread, setSpread] = useState(false);

  // ---- loaded zones -------------------------------------------------------
  const [loaded, setLoaded] = useState<LoadedZone[]>([]);
  const loadedRef = useRef<LoadedZone[]>([]);
  loadedRef.current = loaded;
  const zoneCacheRef = useRef<Map<string, ZoneEntry[]>>(new Map());
  zoneCacheRef.current = zoneCache;
  const [busyPaths, setBusyPaths] = useState<Set<string>>(() => new Set());

  // ---- bulk "Load all" run -------------------------------------------------
  const [bulk, setBulk] = useState<BulkLoadProgress | null>(null);
  const bulkCancelRef = useRef(false);
  const bulkRunningRef = useRef(false);
  /**
   * Zones a "Load all" click queued that still need the user to confirm, with
   * the kind scope the browser applied ("Art", "Art, Gameplay", …) so the
   * dialog can name exactly what is about to load.
   */
  const [confirmLoad, setConfirmLoad] = useState<{ entries: ZoneEntry[]; scope: string } | null>(
    null,
  );

  // ---- edits / selection / view -------------------------------------------
  const [editState, dispatchEdit] = useReducer(editReducer, EMPTY_EDITS);
  // The viewport supports multi-selection; the panels inspect the last picked
  // object (the "primary" selection).
  const [selection, setSelection] = useState<number[]>([]);
  const selectionRef = useRef<number[]>([]);
  selectionRef.current = selection;
  const [mode, setMode] = useState<ToolMode>("translate");
  const [showGeometry, setShowGeometry] = useState(true);
  const [showEntities, setShowEntities] = useState(true);
  const [wireframe, setWireframe] = useState(false);
  const [frameToken, setFrameToken] = useState(0);

  // ---- saving -------------------------------------------------------------
  const [saving, setSaving] = useState(false);
  const [showNewProject, setShowNewProject] = useState(false);
  const [newProjName, setNewProjName] = useState("");
  const [newProjAuthor, setNewProjAuthor] = useState("");

  const rootRef = useRef<HTMLDivElement>(null);
  const viewportRef = useRef<HTMLDivElement>(null);

  const meshLoader = useMemo(
    () => (gameDir ? api.makeMeshLoader(gameDir) : async () => new ArrayBuffer(0)),
    [gameDir],
  );

  // The WebGL viewport is only created once the user has actually opened the
  // tool — every OmniTool tool stays mounted for the lifetime of the window.
  const [mounted, setMounted] = useState(false);
  useEffect(() => {
    if (active) setMounted(true);
  }, [active]);

  /**
   * `<Viewport>` owns its gizmo mode and camera framing internally and exposes
   * no props for them, but its renderer binds the same shortcuts on its canvas.
   * The shell toolbar therefore drives it the way a keypress would.
   */
  const sendViewportKey = useCallback((code: string) => {
    const canvas = viewportRef.current?.querySelector("canvas");
    if (!canvas) return;
    canvas.dispatchEvent(new KeyboardEvent("keydown", { code }));
    canvas.dispatchEvent(new KeyboardEvent("keyup", { code }));
  }, []);

  const applyMode = useCallback(
    (m: ToolMode) => {
      setMode(m);
      sendViewportKey(m === "translate" ? "KeyG" : m === "rotate" ? "KeyR" : "KeyT");
    },
    [sendViewportKey],
  );

  const frameSelection = useCallback(() => {
    setFrameToken((t) => t + 1);
    sendViewportKey(selectionRef.current.length > 0 ? "KeyF" : "KeyH");
  }, [sendViewportKey]);

  const frameAll = useCallback(() => {
    setFrameToken((t) => t + 1);
    sendViewportKey("KeyH");
  }, [sendViewportKey]);

  /* ---------------- region / zone index ---------------- */

  const loadRegions = useCallback(async () => {
    if (!gameDir) return;
    setLoadingRegions(true);
    // Re-indexing is the one action that means "the archives may have changed",
    // so the cached zone lists have to go with them — otherwise `selectRegion`
    // keeps serving a pre-refresh list and the find box keeps searching it.
    setZoneCache(new Map());
    try {
      const list = await api.listPlanets(gameDir);
      setRegions(list);
      pushLog("success", `Indexed ${list.length} level regions.`);
      try {
        setStats(await api.zoneStats(gameDir));
      } catch {
        /* stats are decorative */
      }
    } catch (e) {
      pushLog("error", `Could not index level zones: ${e}`);
    } finally {
      setLoadingRegions(false);
    }
  }, [gameDir, pushLog]);

  const indexedRef = useRef(false);
  useEffect(() => {
    if (!active || !gameDir || indexedRef.current) return;
    indexedRef.current = true;
    void loadRegions();
  }, [active, gameDir, loadRegions]);

  const selectRegion = useCallback(
    async (key: string) => {
      setActiveRegion(key);
      const cached = zoneCacheRef.current.get(key);
      setZones(cached ?? []);
      if (!gameDir || cached) return;
      setLoadingZones(true);
      try {
        const list = await api.listZones(gameDir, key);
        setZones(list);
        setZoneCache((prev) => new Map(prev).set(key, list));
      } catch (e) {
        pushLog("error", `Could not list zones of ${key}: ${e}`);
      } finally {
        setLoadingZones(false);
      }
    },
    [gameDir, pushLog],
  );

  /**
   * Pull in several regions' zone lists at once so the find box can match zone
   * names inside them. Bounded by the browser, which never asks for more than a
   * handful, and skips anything already held.
   */
  const prefetchRegions = useCallback(
    async (keys: string[]) => {
      if (!gameDir) return;
      const todo = keys.filter((k) => !zoneCacheRef.current.has(k));
      if (todo.length === 0) return;
      setLoadingZones(true);
      let zonesSeen = 0;
      try {
        for (const key of todo) {
          try {
            const list = await api.listZones(gameDir, key);
            zonesSeen += list.length;
            setZoneCache((prev) => new Map(prev).set(key, list));
          } catch (e) {
            pushLog("error", `Could not list zones of ${key}: ${e}`);
          }
        }
        pushLog(
          "success",
          `Listed ${zonesSeen.toLocaleString()} zones across ${todo.length} region${
            todo.length === 1 ? "" : "s"
          } — their names are now searchable from the find box.`,
        );
      } finally {
        setLoadingZones(false);
      }
    },
    [gameDir, pushLog],
  );

  /* ---------------- load / unload zones ---------------- */

  const markBusy = (path: string, busy: boolean) =>
    setBusyPaths((prev) => {
      const next = new Set(prev);
      if (busy) next.add(path);
      else next.delete(path);
      return next;
    });

  const loadZonePath = useCallback(
    async (path: string) => {
      if (!gameDir) return;
      markBusy(path, true);
      try {
        const scene = await api.loadZone(gameDir, path);
        setLoaded((prev) => {
          if (prev.some((z) => z.path === path)) return prev;
          const used = new Set(prev.map((z) => z.slot));
          let slot = 0;
          while (used.has(slot)) slot++;
          return [...prev, { path, slot, scene }];
        });
        pushLog(
          "success",
          `${path.split("/").pop()} — ${scene.objects.length.toLocaleString()} objects, ${scene.models.length.toLocaleString()} models`,
        );
      } catch (e) {
        pushLog("error", `Could not load ${path}: ${e}`);
      } finally {
        markBusy(path, false);
      }
    },
    [gameDir, pushLog],
  );

  const unloadZonePath = useCallback((path: string) => {
    setLoaded((prev) => prev.filter((z) => z.path !== path));
  }, []);

  const toggleZone = useCallback(
    (entry: ZoneEntry) => {
      if (loaded.some((z) => z.path === entry.path)) unloadZonePath(entry.path);
      else void loadZonePath(entry.path);
    },
    [loaded, loadZonePath, unloadZonePath],
  );

  const loadTile = useCallback(
    async (entries: ZoneEntry[]) => {
      const missing = entries.filter((e) => !loaded.some((z) => z.path === e.path));
      for (const e of missing) await loadZonePath(e.path);
    },
    [loaded, loadZonePath],
  );

  const unloadTile = useCallback((entries: ZoneEntry[]) => {
    const paths = new Set(entries.map((e) => e.path));
    setLoaded((prev) => prev.filter((z) => !paths.has(z.path)));
  }, []);

  const cancelBulk = useCallback(() => {
    if (!bulkRunningRef.current) return;
    bulkCancelRef.current = true;
    pushLog("info", "Cancelling the zone load — zones already loaded stay loaded.");
  }, [pushLog]);

  const clearZones = useCallback(() => {
    bulkCancelRef.current = true;
    setLoaded([]);
    setSelection([]);
  }, []);

  /**
   * Loads a queue of zones one at a time so the window keeps repainting, keeps
   * a per-zone busy marker in the browser, survives individual failures and
   * can be stopped mid-run without losing what already came in.
   */
  const runBulkLoad = useCallback(
    async (entries: ZoneEntry[], scope?: string) => {
      if (!gameDir || bulkRunningRef.current) return;
      const what = scope ? `${scope} zones` : "zones";

      const already = new Set(loadedRef.current.map((z) => z.path));
      const queue: ZoneEntry[] = [];
      const seen = new Set<string>();
      for (const e of entries) {
        if (!e.hasObjects || already.has(e.path) || seen.has(e.path)) continue;
        seen.add(e.path);
        queue.push(e);
      }
      if (queue.length === 0) {
        pushLog("info", `Nothing to load — every listed ${what.slice(0, -1)} is already loaded or is an empty stub.`);
        return;
      }

      bulkRunningRef.current = true;
      bulkCancelRef.current = false;
      setBulk({ done: 0, total: queue.length, failed: 0 });
      pushLog("info", `Loading ${queue.length.toLocaleString()} ${what}…`);

      let done = 0;
      let failed = 0;
      let objects = 0;
      let batch: { path: string; scene: ZoneScene }[] = [];

      const flush = () => {
        if (batch.length === 0) return;
        const chunk = batch;
        batch = [];
        setLoaded((prev) => {
          const used = new Set(prev.map((z) => z.slot));
          const have = new Set(prev.map((z) => z.path));
          const next = [...prev];
          let slot = 0;
          for (const item of chunk) {
            if (have.has(item.path)) continue;
            while (used.has(slot)) slot++;
            used.add(slot);
            have.add(item.path);
            next.push({ path: item.path, slot, scene: item.scene });
          }
          return next;
        });
      };

      try {
        for (const entry of queue) {
          if (bulkCancelRef.current) break;
          markBusy(entry.path, true);
          try {
            const scene = await api.loadZone(gameDir, entry.path);
            batch.push({ path: entry.path, scene });
            objects += scene.objects.length;
          } catch (e) {
            failed++;
            pushLog("error", `Could not load ${entry.path}: ${e}`);
          } finally {
            markBusy(entry.path, false);
          }
          done++;
          setBulk({ done, total: queue.length, failed });
          if (batch.length >= BULK_FLUSH_EVERY) flush();
        }
        flush();
      } finally {
        bulkRunningRef.current = false;
        setBulk(null);
      }

      const cancelled = bulkCancelRef.current;
      bulkCancelRef.current = false;
      const ok = done - failed;
      const parts = [`${ok.toLocaleString()} of ${queue.length.toLocaleString()} ${what}`];
      if (objects > 0) parts.push(`${objects.toLocaleString()} objects`);
      if (failed > 0) parts.push(`${failed.toLocaleString()} failed`);
      pushLog(
        cancelled || failed > 0 ? "warning" : "success",
        `${cancelled ? "Cancelled — loaded" : "Loaded"} ${parts.join(", ")}.`,
      );

      // Let the composite scene commit and reach the renderer before framing.
      if (ok > 0) window.setTimeout(frameAll, 80);
    },
    [gameDir, pushLog, frameAll],
  );

  /**
   * "Load all" from the zone browser. The browser hands over exactly the zones
   * of the SELECTED KIND CHIPS that its search filter and "Hide stubs" state
   * leave listed, plus the name of that scope; a big queue asks for
   * confirmation first.
   */
  const requestLoadAll = useCallback(
    (entries: ZoneEntry[], scope: string) => {
      const already = new Set(loadedRef.current.map((z) => z.path));
      const queue = entries.filter((e) => e.hasObjects && !already.has(e.path));
      if (queue.length === 0) {
        pushLog(
          "info",
          `Nothing to load — every listed ${scope} zone is already loaded or is an empty stub.`,
        );
        return;
      }
      if (queue.length > BULK_CONFIRM_THRESHOLD) {
        setConfirmLoad({ entries: queue, scope });
        return;
      }
      void runBulkLoad(queue, scope);
    },
    [pushLog, runBulkLoad],
  );

  // "Send to Level Editor" from the Asset Browser: `?assetPath=…zone`.
  const incomingRef = useRef<string | null>(null);
  useEffect(() => {
    if (!active || !gameDir) return;
    const state = location.state as { assetPath?: string } | null;
    const params = new URLSearchParams(location.search);
    const incoming = state?.assetPath ?? params.get("assetPath") ?? undefined;
    if (!incoming || !incoming.toLowerCase().endsWith(".zone")) return;
    const normalised = incoming.replace(/\\/g, "/").toLowerCase();
    if (incomingRef.current === normalised) return;
    incomingRef.current = normalised;
    void loadZonePath(normalised);
  }, [active, gameDir, location, loadZonePath]);

  // Once the index is in, point the browser at the region that zone lives in.
  useEffect(() => {
    const incoming = incomingRef.current;
    if (!incoming || activeRegion || regions.length === 0) return;
    const hit = regions.find((r) => incoming.startsWith(`${r.key}/`));
    if (hit) void selectRegion(hit.key);
  }, [regions, activeRegion, selectRegion]);

  /* ---------------- which region each loaded zone came from ---------------- */

  const regionKeys = useMemo(() => new Set(regions.map((r) => r.key)), [regions]);

  /**
   * Loaded zone path -> region key. The mapping is derived from the path with
   * the same rule the backend uses and then checked against the real region
   * list, so a mismatch shows as "(unknown region)" instead of a wrong label.
   */
  const zoneRegion = useMemo(() => {
    const m = new Map<string, string>();
    for (const z of loaded) {
      m.set(z.path, resolveRegionKey(z.path, regionKeys) ?? "(unknown region)");
    }
    return m;
  }, [loaded, regionKeys]);

  /** One row per region with zones in the viewport, in the order they arrived. */
  const loadedRegions = useMemo<RegionLoadSummary[]>(() => {
    const order: string[] = [];
    const byKey = new Map<string, RegionLoadSummary>();
    for (const z of loaded) {
      const key = zoneRegion.get(z.path) ?? "(unknown region)";
      let row = byKey.get(key);
      if (!row) {
        const meta = regions.find((r) => r.key === key);
        row = {
          key,
          label: meta?.label ?? key,
          group: meta?.group ?? "other",
          zones: 0,
          objects: 0,
          min: null,
          max: null,
        };
        byKey.set(key, row);
        order.push(key);
      }
      row.zones++;
      row.objects += z.scene.objects.length;
      const b = z.scene.bounds;
      if (b) {
        row.min = row.min
          ? [
              Math.min(row.min[0], b.min[0]),
              Math.min(row.min[1], b.min[1]),
              Math.min(row.min[2], b.min[2]),
            ]
          : [...b.min];
        row.max = row.max
          ? [
              Math.max(row.max[0], b.max[0]),
              Math.max(row.max[1], b.max[1]),
              Math.max(row.max[2], b.max[2]),
            ]
          : [...b.max];
      }
    }
    return order.map((k) => byKey.get(k)!);
  }, [loaded, zoneRegion, regions]);

  /**
   * What the loaded regions do to each other in world space, measured rather
   * than assumed: the level carries no per-region placement, so two regions
   * either were authored in one space or were not, and only the zones can say
   * which. Skipped entirely below two regions, which is the common case.
   */
  const regionMix = useMemo<RegionMixReport | null>(() => {
    if (loadedRegions.length < 2) return null;
    const instances: { region: string; modelId: string; x: number; y: number; z: number }[] = [];
    for (const z of loaded) {
      const region = zoneRegion.get(z.path) ?? "(unknown region)";
      for (const o of z.scene.objects) {
        if (o.kind !== KIND_MODEL || !o.modelId || o.modelId === "0x0") continue;
        instances.push({
          region,
          modelId: o.modelId,
          x: o.matrix[12],
          y: o.matrix[13],
          z: o.matrix[14],
        });
      }
    }
    return analyseRegionMix(loadedRegions, instances);
  }, [loaded, loadedRegions, zoneRegion]);

  const spreadStepMetres = useMemo(() => spreadStep(loadedRegions), [loadedRegions]);

  /**
   * Region key -> the artificial X shift the viewport draws it at. All zeros
   * unless the user turned "Lay regions out side by side" on.
   */
  const spreadOffsets = useMemo(() => {
    const m = new Map<string, number>();
    if (!spread || loadedRegions.length < 2) return m;
    const step = Math.max(SPREAD_MIN_METRES, spreadStepMetres);
    loadedRegions.forEach((r, i) => m.set(r.key, i * step));
    return m;
  }, [spread, loadedRegions, spreadStepMetres]);

  /** The X shift applied to one loaded zone, 0 when the layout is off. */
  const spreadOf = useCallback(
    (zonePath: string) => spreadOffsets.get(zoneRegion.get(zonePath) ?? "") ?? 0,
    [spreadOffsets, zoneRegion],
  );

  const unloadRegion = useCallback(
    (key: string) => {
      const doomed = loadedRef.current.filter(
        (z) => (resolveRegionKey(z.path, regionKeys) ?? "(unknown region)") === key,
      );
      if (doomed.length === 0) return;
      const paths = new Set(doomed.map((z) => z.path));
      setLoaded((prev) => prev.filter((z) => !paths.has(z.path)));
      pushLog("info", `Unloaded ${doomed.length} zone${doomed.length === 1 ? "" : "s"} of ${key}.`);
    },
    [regionKeys, pushLog],
  );

  /* ---------------- composite scene ---------------- */

  const scene = useMemo<ZoneScene | null>(() => {
    if (loaded.length === 0) return null;
    const edits = editState.edits;
    const objects: SceneObjectDto[] = [];
    const actors: ZoneScene["actors"] = [];
    const sections: ZoneScene["sections"] = [];
    const models = new Map<string, { id: string; path: string; instanceCount: number }>();
    let min: [number, number, number] | null = null;
    let max: [number, number, number] | null = null;

    for (const z of loaded) {
      const base = z.slot * OFFSET_STRIDE;
      // Display-only X shift, non-zero only while "Lay regions out side by
      // side" is on. It never reaches an edit or a save: `commitTransform`
      // refuses while it is on, and the pending edits already in hand were
      // recorded in real zone space.
      const dx = spreadOf(z.path);
      for (const o of z.scene.objects) {
        const edit = edits.get(editKey(z.path, o.offset));
        // An impostor's or decal's mesh is addressed by its own zone path and
        // its own offset. Both are gone once the object is in the composite, so
        // the key is stamped on here, while they are still in hand.
        const geometryKey = o.geometry ? zoneGeometryKey(z.path, o.offset) : undefined;
        if (!edit && base === 0 && !geometryKey && dx === 0) {
          objects.push(o);
        } else {
          const source = edit ? edit.matrix : o.matrix;
          const matrix = dx === 0 ? source : source.slice();
          if (dx !== 0) matrix[12] += dx;
          objects.push({
            ...o,
            offset: o.offset + base,
            matrix,
            scale: edit ? edit.scale : o.scale,
            ...(geometryKey ? { geometryKey } : {}),
          });
        }
      }
      for (const a of z.scene.actors) {
        actors.push(base === 0 ? a : { ...a, sceneOffset: a.sceneOffset + base });
      }
      for (const s of z.scene.sections) sections.push(s);
      for (const m of z.scene.models) {
        const hit = models.get(m.id);
        if (hit) hit.instanceCount += m.instanceCount;
        else models.set(m.id, { ...m });
      }
      const b = z.scene.bounds;
      if (b) {
        const bmin: [number, number, number] = [b.min[0] + dx, b.min[1], b.min[2]];
        const bmax: [number, number, number] = [b.max[0] + dx, b.max[1], b.max[2]];
        min = min ? ([
          Math.min(min[0], bmin[0]),
          Math.min(min[1], bmin[1]),
          Math.min(min[2], bmin[2]),
        ] as [number, number, number]) : bmin;
        max = max ? ([
          Math.max(max[0], bmax[0]),
          Math.max(max[1], bmax[1]),
          Math.max(max[2], bmax[2]),
        ] as [number, number, number]) : bmax;
      }
    }

    const first = loaded[0].scene;
    return {
      path: loaded.map((z) => z.path).join(" + "),
      id: first.id,
      sizes: first.sizes,
      dirty: edits.size > 0,
      bounds: min && max ? { min, max } : null,
      models: [...models.values()].sort((a, b) => b.instanceCount - a.instanceCount),
      objects,
      actors,
      sections,
    };
  }, [loaded, editState.edits, spreadOf]);

  /** synthetic offset -> which zone the object came from and its real offset */
  const objectIndex = useMemo(() => {
    const map = new Map<number, { zonePath: string; object: SceneObjectDto }>();
    for (const z of loaded) {
      const base = z.slot * OFFSET_STRIDE;
      for (const o of z.scene.objects) map.set(o.offset + base, { zonePath: z.path, object: o });
    }
    return map;
  }, [loaded]);

  const modelPaths = useMemo(() => {
    const m = new Map<string, string>();
    for (const model of scene?.models ?? []) m.set(model.id, model.path);
    return m;
  }, [scene]);

  const modelCounts = useMemo(() => {
    const m = new Map<string, number>();
    for (const model of scene?.models ?? []) m.set(model.id, model.instanceCount);
    return m;
  }, [scene]);

  /** Synthetic offsets that currently carry a pending edit. */
  const editedOffsets = useMemo(() => {
    const out = new Set<number>();
    for (const z of loaded) {
      const base = z.slot * OFFSET_STRIDE;
      for (const key of editState.edits.keys()) {
        if (!key.startsWith(`${z.path}#`)) continue;
        const off = Number(key.slice(z.path.length + 1));
        if (Number.isFinite(off)) out.add(off + base);
      }
    }
    return out;
  }, [loaded, editState.edits]);

  const primary = selection.length > 0 ? selection[selection.length - 1] : null;
  const selectedRecord = primary === null ? undefined : objectIndex.get(primary);
  const selectedObject = useMemo<SceneObjectDto | null>(() => {
    if (primary === null || !selectedRecord) return null;
    return scene?.objects.find((o) => o.offset === primary) ?? null;
  }, [scene, primary, selectedRecord]);

  const selectedActor = useMemo(() => {
    if (primary === null || !scene) return null;
    return scene.actors.find((a) => a.sceneOffset === primary) ?? null;
  }, [scene, primary]);

  const selectedSections = useMemo(() => {
    if (!selectedRecord) return scene?.sections ?? [];
    const zone = loaded.find((z) => z.path === selectedRecord.zonePath);
    return zone?.scene.sections ?? [];
  }, [selectedRecord, loaded, scene]);

  /* ---------------- edits ---------------- */

  const commitTransform = useCallback(
    (syntheticOffset: number, value: TransformValue) => {
      const rec = objectIndex.get(syntheticOffset);
      if (!rec) return;
      // While the side-by-side layout is on, the viewport's coordinates are the
      // tool's invention, so a gizmo drag would write an invented position into
      // a real zone. Refuse rather than guess. Edits made before it was turned
      // on are untouched and still save.
      if (spread && spreadOf(rec.zonePath) !== 0) {
        pushLog(
          "warning",
          "Transform edits are off while regions are laid out side by side — the X offset is this tool's own, not the game's. Turn the layout off to edit.",
        );
        return;
      }
      dispatchEdit({
        type: "edit",
        key: editKey(rec.zonePath, rec.object.offset),
        original: { matrix: rec.object.matrix, scale: rec.object.scale },
        to: value,
      });
    },
    [objectIndex, spread, spreadOf, pushLog],
  );

  /** Viewport picks: `additive` extends the current selection (ctrl / box-select). */
  const handleViewportSelect = useCallback((offsets: number[], additive: boolean) => {
    setSelection((prev) => {
      if (!additive) return offsets;
      const merged = new Set(prev);
      for (const o of offsets) {
        if (merged.has(o)) merged.delete(o);
        else merged.add(o);
      }
      return [...merged];
    });
  }, []);

  /**
   * Transform callback for the viewport gizmo. Deliberately permissive about
   * its argument shape: `(offset, matrix)`, `(offset, matrix, scale)` and
   * `(offset, { matrix, scale })` are all accepted.
   */
  const handleViewportTransform = useCallback(
    (syntheticOffset: number, a: unknown, b?: unknown) => {
      const rec = objectIndex.get(syntheticOffset);
      if (!rec) return;
      let matrix: number[] | undefined;
      let scale: [number, number, number] | undefined;
      if (Array.isArray(a)) {
        matrix = a as number[];
        if (Array.isArray(b)) scale = b as [number, number, number];
      } else if (a && typeof a === "object") {
        const obj = a as { matrix?: number[]; scale?: [number, number, number] };
        matrix = obj.matrix;
        scale = obj.scale;
      }
      if (!matrix && !scale) return;
      // Keep whichever half the gizmo did not touch at its currently edited
      // value, not at the value the zone shipped with.
      const current = editState.edits.get(editKey(rec.zonePath, rec.object.offset));
      commitTransform(syntheticOffset, {
        matrix: matrix ?? current?.matrix ?? rec.object.matrix,
        scale: scale ?? current?.scale ?? rec.object.scale,
      });
    },
    [objectIndex, commitTransform, editState.edits],
  );

  const handleInspectorTransform = useCallback(
    (next: TransformValue) => {
      if (primary === null) return;
      commitTransform(primary, next);
    },
    [primary, commitTransform],
  );

  const revertSelected = useCallback(() => {
    if (primary === null) return;
    const rec = objectIndex.get(primary);
    if (!rec) return;
    dispatchEdit({
      type: "revert",
      key: editKey(rec.zonePath, rec.object.offset),
      original: { matrix: rec.object.matrix, scale: rec.object.scale },
    });
  }, [primary, objectIndex]);

  const selectedEdited = primary !== null && editedOffsets.has(primary);

  /* ---------------- select all / none ---------------- */

  /** Fast membership test for the tree; the array stays the source of truth. */
  const selectedSet = useMemo(() => new Set(selection), [selection]);

  /**
   * Selects every object the viewport is currently drawing. Objects the
   * renderer has no instance for — hidden by the geometry/entity toggles,
   * model instances without a model, or objects with a broken matrix — are
   * skipped so the count matches what is actually highlighted.
   */
  const selectAll = useCallback(() => {
    const objects = scene?.objects ?? [];
    const out: number[] = [];
    let skipped = 0;
    for (const o of objects) {
      const visible = o.kind === KIND_MODEL ? showGeometry && o.modelId !== "0x0" : showEntities;
      if (!visible || !o.matrix.every(Number.isFinite)) continue;
      if (out.length >= SELECT_ALL_LIMIT) {
        skipped++;
        continue;
      }
      out.push(o.offset);
    }
    setSelection(out);
    if (out.length === 0) {
      pushLog("info", "Nothing to select — no visible objects in the loaded zones.");
    } else if (skipped > 0) {
      pushLog(
        "warning",
        `Selected the first ${out.length.toLocaleString()} visible objects — ${skipped.toLocaleString()} more were left out to keep the viewport responsive.`,
      );
    } else {
      pushLog("info", `Selected ${out.length.toLocaleString()} objects.`);
    }
  }, [scene, showGeometry, showEntities, pushLog]);

  const selectNone = useCallback(() => setSelection([]), []);

  // The window key handler is bound once per activation; reach the latest
  // implementation through a ref instead of re-binding on every scene change.
  const selectAllRef = useRef(selectAll);
  selectAllRef.current = selectAll;

  /* ---------------- keyboard shortcuts ---------------- */

  useEffect(() => {
    if (!active) return;
    const onKey = (e: KeyboardEvent) => {
      // The tool stays mounted while another tool is on screen — only react
      // when this page is actually visible, and never while typing.
      if (!rootRef.current || rootRef.current.offsetParent === null) return;
      const target = e.target as HTMLElement | null;
      if (target && (/^(INPUT|TEXTAREA|SELECT)$/.test(target.tagName) || target.isContentEditable))
        return;

      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "a") {
        // Ctrl/Cmd+A would otherwise select the whole page's text.
        e.preventDefault();
        selectAllRef.current();
        return;
      }
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "z") {
        e.preventDefault();
        dispatchEdit({ type: e.shiftKey ? "redo" : "undo" });
        return;
      }
      // G / R / T / F / H / Escape belong to the viewport canvas; mirror the
      // gizmo keys into the toolbar so both stay in sync, and keep the shell's
      // own toggles on keys the renderer does not claim.
      switch (e.code) {
        case "KeyG": setMode("translate"); break;
        case "KeyR": setMode("rotate"); break;
        case "KeyT": setMode("scale"); break;
        case "Digit1": setShowGeometry((v) => !v); break;
        case "Digit2": setShowEntities((v) => !v); break;
        case "Digit3": setWireframe((v) => !v); break;
        case "Escape": setSelection([]); break;
        default: return;
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [active]);

  /* ---------------- save ---------------- */

  const pendingByZone = useMemo(() => {
    const byZone = new Map<string, ZoneEdit[]>();
    for (const [key, value] of editState.edits) {
      const hash = key.lastIndexOf("#");
      const zonePath = key.slice(0, hash);
      const offset = Number(key.slice(hash + 1));
      const list = byZone.get(zonePath) ?? [];
      list.push({ offset, matrix: value.matrix, scale: value.scale });
      byZone.set(zonePath, list);
    }
    for (const list of byZone.values()) list.sort((a, b) => a.offset - b.offset);
    return byZone;
  }, [editState.edits]);

  const handleCreateProject = useCallback(async () => {
    const name = newProjName.trim();
    if (!name) return;
    try {
      await createProject(name, newProjAuthor.trim() || "Unknown");
      pushLog("success", `Created project "${name}".`);
      setShowNewProject(false);
      setNewProjName("");
      setNewProjAuthor("");
    } catch (e) {
      pushLog("error", `Could not create project: ${e}`);
    }
  }, [newProjName, newProjAuthor, createProject, pushLog]);

  const handleSave = useCallback(async () => {
    if (!gameDir || pendingByZone.size === 0) return;
    if (!selectedProject) {
      pushLog("error", "Pick or create a project to save into first.");
      return;
    }
    setSaving(true);
    try {
      let files = 0;
      for (const [zonePath, edits] of pendingByZone) {
        const res = await api.saveZone(gameDir, zonePath, selectedProject, edits);
        files++;
        pushLog(
          "success",
          `${res.assetPath} — ${edits.length} edit${edits.length === 1 ? "" : "s"}, ${res.bytesWritten.toLocaleString()} bytes → ${res.projectFile}`,
        );
      }
      dispatchEdit({ type: "reset" });
      pushLog(
        "info",
        `Wrote ${files} zone file${files === 1 ? "" : "s"} into "${selectedProject}". Open the Stager to export the project as a .stage mod.`,
      );
    } catch (e) {
      pushLog("error", `Save failed: ${e}`);
    } finally {
      setSaving(false);
    }
  }, [gameDir, pendingByZone, selectedProject, pushLog]);

  /* ---------------- derived counters ---------------- */

  const loadedPaths = useMemo(() => new Set(loaded.map((z) => z.path)), [loaded]);
  const objectCounts = useMemo(() => {
    const m = new Map<string, number>();
    for (const z of loaded) m.set(z.path, z.scene.objects.length);
    return m;
  }, [loaded]);

  // The artificial layout only makes sense with something to lay out.
  useEffect(() => {
    if (spread && loadedRegions.length < 2) setSpread(false);
  }, [spread, loadedRegions.length]);

  /**
   * The worst thing the loaded regions do to each other, in one line, so the
   * status bar can carry it without the user having to look at the browser.
   */
  const regionNote = useMemo(() => {
    if (spread) {
      return `Regions laid out +${spreadStepMetres.toLocaleString()} m apart — this tool's offset, not the game's; edits off`;
    }
    const worst = regionMix?.pairs[0];
    if (!worst || worst.relation === "apart") return null;
    return worst.relation === "coincident"
      ? `${worst.duplicates.toLocaleString()} instances are drawn twice — two regions hold the same objects at the same coordinates`
      : "Two loaded regions occupy overlapping coordinates";
  }, [spread, spreadStepMetres, regionMix]);

  /** Tell the log the first time a second region joins the view. */
  const announcedMixRef = useRef<string>("");
  useEffect(() => {
    if (loadedRegions.length < 2 || !regionMix) return;
    const sig = loadedRegions.map((r) => r.key).join("|");
    if (announcedMixRef.current === sig) return;
    announcedMixRef.current = sig;
    const worst = regionMix.pairs[0];
    if (!worst) return;
    const shortName = (k: string) => k.split("/").slice(2).join("/") || k;
    pushLog(
      worst.relation === "apart" ? "info" : "warning",
      relationSentence(worst, shortName),
    );
  }, [loadedRegions, regionMix, pushLog]);

  const instanceCount = scene?.objects.filter((o) => o.kind === 0).length ?? 0;
  const entityCount = (scene?.objects.length ?? 0) - instanceCount;

  const selectionLabel =
    selection.length > 1
      ? `${selection.length.toLocaleString()} objects selected`
      : selectedObject
        ? `${selectedObject.kindName}: ${selectedObject.name || `@0x${selectedObject.offset.toString(16)}`}`
        : null;

  const busyLabel = bulk
    ? `Loading ${bulk.done.toLocaleString()} / ${bulk.total.toLocaleString()} zones…${
        bulk.failed > 0 ? ` (${bulk.failed} failed)` : ""
      }`
    : busyPaths.size > 0
      ? `Loading ${busyPaths.size} zone(s)…`
      : null;

  /* ---------------- render ---------------- */

  if (!gameDir) {
    return (
      <div className={styles.page} ref={rootRef}>
        <Header
          pendingEdits={0}
          projects={[]}
          selectedProject=""
          setSelectedProject={() => {}}
          onSave={() => {}}
          saving={false}
          disabled
          showNewProject={false}
          setShowNewProject={() => {}}
          newProjName=""
          newProjAuthor=""
          setNewProjName={() => {}}
          setNewProjAuthor={() => {}}
          onCreateProject={() => {}}
        />
        <div className={styles.setup}>
          <p>The Level Editor needs the game directory to read zone and model assets.</p>
          <button className={styles.primary} onClick={() => setSettingsOpen(true)}>
            <LuSettings /> Open Settings
          </button>
          <p className={styles.setupHint}>
            Set <b>Archives directory</b> to your <code>Ratchet &amp; Clank - Rift Apart</code>{" "}
            install folder. The game is only ever read from.
          </p>
        </div>
      </div>
    );
  }

  return (
    <div className={styles.page} ref={rootRef}>
      <Header
        pendingEdits={editState.edits.size}
        projects={projects.map((p) => p.name)}
        selectedProject={selectedProject}
        setSelectedProject={setSelectedProject}
        onSave={handleSave}
        saving={saving}
        disabled={editState.edits.size === 0}
        showNewProject={showNewProject}
        setShowNewProject={setShowNewProject}
        newProjName={newProjName}
        newProjAuthor={newProjAuthor}
        setNewProjName={setNewProjName}
        setNewProjAuthor={setNewProjAuthor}
        onCreateProject={handleCreateProject}
      />

      <div className={styles.body}>
        <aside className={styles.left}>
          <ZoneBrowser
            regions={regions}
            stats={stats}
            activeRegion={activeRegion}
            zones={zones}
            loadedPaths={loadedPaths}
            busyPaths={busyPaths}
            loadingRegions={loadingRegions}
            loadingZones={loadingZones}
            objectCounts={objectCounts}
            zoneCache={zoneCache}
            loadedRegions={loadedRegions}
            mix={regionMix}
            onUnloadRegion={unloadRegion}
            onPrefetchRegions={(keys) => void prefetchRegions(keys)}
            spread={spread}
            spreadStepMetres={spreadStepMetres}
            onToggleSpread={() => setSpread((v) => !v)}
            onSelectRegion={(k) => void selectRegion(k)}
            onToggleZone={toggleZone}
            onLoadTile={(entries) => void loadTile(entries)}
            onUnloadTile={unloadTile}
            onLoadAll={requestLoadAll}
            onCancelBulk={cancelBulk}
            bulk={bulk}
            onClearAll={clearZones}
            onRefresh={() => void loadRegions()}
          />
        </aside>

        <section className={styles.centre}>
          <Toolbar
            mode={mode}
            onMode={applyMode}
            showGeometry={showGeometry}
            showEntities={showEntities}
            wireframe={wireframe}
            onToggleGeometry={() => setShowGeometry((v) => !v)}
            onToggleEntities={() => setShowEntities((v) => !v)}
            onToggleWireframe={() => setWireframe((v) => !v)}
            onFrameSelection={frameSelection}
            canFrame={!!scene}
            canUndo={editState.index > 0}
            canRedo={editState.index < editState.history.length}
            onUndo={() => dispatchEdit({ type: "undo" })}
            onRedo={() => dispatchEdit({ type: "redo" })}
          />

          <div className={styles.viewport} ref={viewportRef}>
            {mounted && (
              <Viewport
                scene={scene}
                meshLoader={meshLoader}
                selection={selection}
                onSelect={handleViewportSelect}
                onTransform={handleViewportTransform}
                showEntities={showEntities}
                showGeometry={showGeometry}
                wireframe={wireframe}
                /* Forward-compatible: ignored by the current Viewport, which
                   owns its gizmo mode. Until it accepts them the shell drives
                   the gizmo through `sendViewportKey` below. */
                {...({ gizmoMode: mode, frameToken } as any)}
              />
            )}
            {!scene && (
              <div className={styles.viewportHint}>
                Pick a region on the left, then load a tile — its <code>_art</code> zone holds the
                map geometry and its <code>_gp</code> zone the gameplay entities, so load both
                together with <b>Load tile</b>.
              </div>
            )}
          </div>

          <StatusBar
            zoneCount={loaded.length}
            regionCount={loadedRegions.length}
            regionNote={regionNote}
            objectCount={scene?.objects.length ?? 0}
            instanceCount={instanceCount}
            entityCount={entityCount}
            modelCount={scene?.models.length ?? 0}
            selectionLabel={selectionLabel}
            pendingEdits={editState.edits.size}
            busy={busyLabel}
          />
        </section>

        <aside className={styles.right}>
          <div className={styles.rightTree}>
            <SceneTree
              objects={scene?.objects ?? []}
              modelPaths={modelPaths}
              selected={primary}
              onSelect={(offset) => setSelection(offset === null ? [] : [offset])}
              editedOffsets={editedOffsets}
              zoneOf={(off) => objectIndex.get(off)?.zonePath}
              selectedSet={selectedSet}
              onSelectAll={selectAll}
              onSelectNone={selectNone}
            />
          </div>
          <div className={styles.rightInspector}>
            <Inspector
              object={selectedObject}
              zonePath={selectedRecord?.zonePath ?? null}
              modelPath={selectedObject ? modelPaths.get(selectedObject.modelId) ?? null : null}
              modelInstanceCount={
                selectedObject ? modelCounts.get(selectedObject.modelId) ?? null : null
              }
              actor={selectedActor}
              sections={selectedSections}
              edited={selectedEdited}
              onTransform={handleInspectorTransform}
              onRevert={revertSelected}
            />
          </div>
        </aside>
      </div>

      {confirmLoad && (
        <ConfirmLoadAll
          count={confirmLoad.entries.length}
          scope={confirmLoad.scope}
          region={activeRegion}
          onCancel={() => setConfirmLoad(null)}
          onConfirm={() => {
            const { entries, scope } = confirmLoad;
            setConfirmLoad(null);
            void runBulkLoad(entries, scope);
          }}
        />
      )}

      <StatusLog entries={log} />
    </div>
  );
}

/* ------------------------------------------------------------------ */

interface ConfirmLoadAllProps {
  count: number;
  /** Kinds the zone browser scoped the click to, e.g. "Art" or "Art, Lighting". */
  scope: string;
  region: string | null;
  onCancel: () => void;
  onConfirm: () => void;
}

function ConfirmLoadAll({ count, scope, region, onCancel, onConfirm }: ConfirmLoadAllProps) {
  return (
    <div
      className={styles.backdrop}
      onClick={(e) => {
        if (e.target === e.currentTarget) onCancel();
      }}
    >
      <div className={styles.modal}>
        <h3 className={styles.modalTitle}>
          Load {count.toLocaleString()} {scope} zone{count === 1 ? "" : "s"}?
        </h3>
        <p className={styles.modalBody}>
          This loads the <b>{scope}</b> zones the browser currently lists
          {region ? (
            <>
              {" "}
              for <b>{region.split("/").pop()}</b>
            </>
          ) : null}{" "}
          — <b>{count.toLocaleString()}</b> zone{count === 1 ? "" : "s"} of that kind that are not
          loaded yet. Zones of every other kind are left alone. Zones load one at a time and the run
          can be stopped from the panel header; everything loaded up to that point is kept.
        </p>
        <p className={styles.modalHint}>
          Narrow the search filter, or turn off a kind chip, to load fewer zones.
        </p>
        <div className={styles.modalActions}>
          <button className={styles.cancelBtn} onClick={onCancel}>
            Cancel
          </button>
          <button className={styles.primary} onClick={onConfirm} autoFocus>
            <LuLayers3 />
            Load {count.toLocaleString()} {scope} zone{count === 1 ? "" : "s"}
          </button>
        </div>
      </div>
    </div>
  );
}

/* ------------------------------------------------------------------ */

interface HeaderProps {
  pendingEdits: number;
  projects: string[];
  selectedProject: string;
  setSelectedProject: (name: string) => void;
  onSave: () => void;
  saving: boolean;
  disabled: boolean;
  showNewProject: boolean;
  setShowNewProject: (v: boolean) => void;
  newProjName: string;
  newProjAuthor: string;
  setNewProjName: (v: string) => void;
  setNewProjAuthor: (v: string) => void;
  onCreateProject: () => void;
}

function Header({
  pendingEdits,
  projects,
  selectedProject,
  setSelectedProject,
  onSave,
  saving,
  disabled,
  showNewProject,
  setShowNewProject,
  newProjName,
  newProjAuthor,
  setNewProjName,
  setNewProjAuthor,
  onCreateProject,
}: HeaderProps) {
  return (
    <div className={styles.header}>
      <div className={styles.headerTitle}>
        <h2 className={styles.title}>Level Editor</h2>
        <p className={styles.subtitle}>View and edit level geometry and gameplay entities</p>
      </div>

      <div className={styles.headerActions}>
        {pendingEdits > 0 && (
          <span className={styles.dirtyBadge}>
            {pendingEdits} unsaved edit{pendingEdits === 1 ? "" : "s"}
          </span>
        )}
        {showNewProject ? (
          <div className={styles.newProject}>
            <input
              className={styles.projInput}
              placeholder="Project name"
              value={newProjName}
              onChange={(e) => setNewProjName(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && onCreateProject()}
              autoFocus
            />
            <input
              className={styles.projInput}
              placeholder="Author"
              value={newProjAuthor}
              onChange={(e) => setNewProjAuthor(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && onCreateProject()}
            />
            <button className={styles.ghost} onClick={onCreateProject} disabled={!newProjName.trim()}>
              Create
            </button>
            <button className={styles.ghost} onClick={() => setShowNewProject(false)}>
              Cancel
            </button>
          </div>
        ) : (
          <>
            <select
              className={styles.projectSelect}
              value={selectedProject}
              onChange={(e) => setSelectedProject(e.target.value)}
              title="Project the edited zone is written into"
            >
              {projects.length === 0 && <option value="">No projects</option>}
              {projects.map((p) => (
                <option key={p} value={p}>
                  {p}
                </option>
              ))}
            </select>
            <button className={styles.ghost} onClick={() => setShowNewProject(true)}>
              + New
            </button>
          </>
        )}
        <button className={styles.primary} onClick={onSave} disabled={disabled || saving}>
          <LuSave />
          {saving ? "Saving…" : "Save to project"}
        </button>
      </div>
    </div>
  );
}
