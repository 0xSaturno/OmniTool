import { useCallback, useEffect, useMemo, useRef, useState, type ReactElement } from "react";
import {
  FiBox,
  FiCompass,
  FiCrosshair,
  FiGlobe,
  FiLayers,
  FiMaximize,
  FiMaximize2,
  FiImage,
  FiMove,
  FiRotateCw,
  FiSlash,
  FiSun,
} from "react-icons/fi";
import { LevelRenderer, KIND_COLORS, type HudState } from "./viewport/renderer";
import type { ShadingMode } from "./viewport/materials";
import { TEXTURE_BUDGET_BYTES, TEXTURE_MAX_SIZE } from "./viewport/textureCache";
import { defaultTextureSource } from "./api";
import {
  KIND_DECAL,
  KIND_IMPOSTOR,
  KIND_MODEL,
  kindName,
  type GizmoMode,
  type MeshLoader,
  type SceneObjectDto,
  type TextureSource,
  type ZoneScene,
} from "./types";
import styles from "./Viewport.module.css";

export interface ViewportProps {
  scene: ZoneScene | null;
  /** Returns the RCMS buffer for a model asset id (`"0x…"`). */
  meshLoader: MeshLoader;
  /**
   * Material tables and texture bytes. Defaults to the Tauri-backed source for
   * the game directory the shell last built a mesh loader for, so the shell
   * needs no new prop; pass one explicitly to mock or disable texturing.
   */
  textureSource?: TextureSource | null;
  /** Selected scene-object offsets. */
  selection: number[];
  onSelect: (offsets: number[], additive: boolean) => void;
  /** Emitted for every affected object after a gizmo drag; matrix is ROW-MAJOR. */
  onTransform?: (offset: number, matrix: number[]) => void;
  showEntities: boolean;
  showGeometry: boolean;
  wireframe?: boolean;
}

const EMPTY_HUD: HudState = {
  fps: 0,
  drawCalls: 0,
  triangles: 0,
  instances: 0,
  entities: 0,
  models: 0,
  loading: 0,
  cameraPos: [0, 0, 0],
  hovered: null,
  selectedCount: 0,
  selectedName: null,
  mirrored: 0,
  windingSuspect: 0,
  blockGeo: 0,
  blockGeoLoaded: 0,
  blockGeoTriangles: 0,
  textures: 0,
  textureBytes: 0,
  textureLoading: 0,
  textureSkipped: 0,
  texturedModels: 0,
};

/** Mirrors `hasBlockGeometry` in the renderer — see the note there. */
function ownsBlockGeometry(o: SceneObjectDto): boolean {
  if (o.kind !== KIND_DECAL && o.kind !== KIND_IMPOSTOR) return false;
  const g = o.geometry;
  return Boolean(g && g.indexCount >= 3 && g.vertexCount >= 3);
}

const SHADING_BUTTONS: { mode: ShadingMode; icon: ReactElement; label: string; title: string }[] = [
  {
    mode: "textured",
    icon: <FiImage />,
    label: "textured",
    title: `Material albedo, lit (N) — requested at up to ${TEXTURE_MAX_SIZE}px`,
  },
  { mode: "lit", icon: <FiSun />, label: "lit", title: "Flat per-model hue, lit (N)" },
  { mode: "normals", icon: <FiCompass />, label: "normals", title: "View-space normals as RGB (N)" },
  { mode: "facing", icon: <FiLayers />, label: "facing", title: "Green = outward face, magenta = inward (N)" },
];

function fmt(n: number): string {
  return n.toLocaleString("en-US");
}

export default function Viewport(props: ViewportProps) {
  const { scene, meshLoader, selection, onSelect, onTransform, showEntities, showGeometry, wireframe } = props;

  // `undefined` means "use the default source"; an explicit `null` disables
  // texturing (the mock source and the tests pass null). `meshLoader` is in the
  // deps because the shell rebuilds it exactly when the game directory changes,
  // which is also when the default source has to be rebound.
  const textureSource = useMemo(
    () => (props.textureSource === undefined ? defaultTextureSource() : props.textureSource),
    [props.textureSource, meshLoader],
  );

  const hostRef = useRef<HTMLDivElement | null>(null);
  const rendererRef = useRef<LevelRenderer | null>(null);

  // Latest callbacks/props behind refs so the renderer is created exactly once.
  const onSelectRef = useRef(onSelect);
  const onTransformRef = useRef(onTransform);
  const meshLoaderRef = useRef(meshLoader);
  onSelectRef.current = onSelect;
  onTransformRef.current = onTransform;
  meshLoaderRef.current = meshLoader;

  const [hud, setHud] = useState<HudState>(EMPTY_HUD);
  const [marquee, setMarquee] = useState<{ x: number; y: number; w: number; h: number } | null>(null);
  const [gizmoMode, setGizmoMode] = useState<GizmoMode | null>("translate");
  const [space, setSpace] = useState<"world" | "local">("world");
  const [shading, setShading] = useState<ShadingMode>("textured");
  const [ready, setReady] = useState(false);

  // -- renderer lifecycle ---------------------------------------------------

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;

    const r = new LevelRenderer({
      container: host,
      onSelect: (offsets, additive) => onSelectRef.current(offsets, additive),
      onTransform: (offset, matrix) => onTransformRef.current?.(offset, matrix),
      onHud: setHud,
      onBoxSelect: setMarquee,
    });
    rendererRef.current = r;
    setReady(true);

    return () => {
      rendererRef.current = null;
      setReady(false);
      r.dispose();
    };
  }, []);

  // -- scene ----------------------------------------------------------------

  const selectionRef = useRef(selection);
  selectionRef.current = selection;

  const textureSourceRef = useRef(textureSource);
  textureSourceRef.current = textureSource;

  useEffect(() => {
    if (!ready) return;
    const r = rendererRef.current;
    if (!r) return;
    r.setScene(scene, (id) => meshLoaderRef.current(id), textureSourceRef.current);
    // A rebuild drops the renderer's selection; re-apply what the shell holds.
    r.setSelection(selectionRef.current);
  }, [ready, scene]);

  // -- selection ------------------------------------------------------------

  useEffect(() => {
    if (!ready) return;
    rendererRef.current?.setSelection(selection);
    // The shell only ever hands over a fresh array when the selection really
    // changed, so identity is the compare — joining the offsets into a key
    // would allocate a megabyte-sized string on every render of a select-all.
  }, [ready, selection]);

  // -- toggles --------------------------------------------------------------

  useEffect(() => {
    if (!ready) return;
    rendererRef.current?.setVisibility({
      showGeometry,
      showEntities,
      wireframe: Boolean(wireframe),
    });
  }, [ready, showGeometry, showEntities, wireframe]);

  useEffect(() => {
    if (!ready) return;
    rendererRef.current?.setGizmoMode(gizmoMode);
  }, [ready, gizmoMode]);

  useEffect(() => {
    if (!ready) return;
    rendererRef.current?.setGizmoSpace(space);
  }, [ready, space]);

  useEffect(() => {
    if (!ready) return;
    rendererRef.current?.setShadingMode(shading);
  }, [ready, shading]);

  // `N` cycles the shading modes. The canvas keeps its own key handling for
  // camera and gizmo shortcuts, so this listens on the window and ignores
  // anything typed into a field.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.code !== "KeyN" || e.ctrlKey || e.metaKey || e.altKey) return;
      const el = e.target as HTMLElement | null;
      const tag = el?.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || el?.isContentEditable) return;
      setShading((m) => {
        const i = SHADING_BUTTONS.findIndex((b) => b.mode === m);
        return SHADING_BUTTONS[(i + 1) % SHADING_BUTTONS.length].mode;
      });
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  // -- actions --------------------------------------------------------------

  const frameAll = useCallback(() => rendererRef.current?.frameAll(), []);
  const frameSelection = useCallback(() => rendererRef.current?.frameSelection(), []);

  // Kinds drawn as markers follow `showEntities`; kinds drawn from block-2
  // geometry follow `showGeometry`, so the legend has to split them or it
  // would name swatches that are not on screen.
  const { markerKinds, meshKinds } = useMemo(() => {
    const markers = new Set<number>();
    const meshes = new Set<number>();
    if (scene) {
      for (const o of scene.objects) {
        if (o.kind === KIND_MODEL) continue;
        (ownsBlockGeometry(o) ? meshes : markers).add(o.kind);
      }
    }
    const sort = (s: Set<number>) => [...s].sort((a, b) => a - b);
    return { markerKinds: sort(markers), meshKinds: sort(meshes) };
  }, [scene]);

  const legendRows = useMemo(() => {
    const rows: { key: string; kind: number; label: string }[] = [];
    if (showGeometry) for (const k of meshKinds) rows.push({ key: `m${k}`, kind: k, label: `${kindName(k)} mesh` });
    if (showEntities) for (const k of markerKinds) rows.push({ key: `e${k}`, kind: k, label: kindName(k) });
    return rows;
  }, [markerKinds, meshKinds, showEntities, showGeometry]);

  const hasSelection = selection.length > 0;
  const [cx, cy, cz] = hud.cameraPos;

  return (
    <div className={styles.viewport}>
      <div className={styles.canvasHost} ref={hostRef} />

      <div className={styles.toolbar}>
        <button className={styles.btn} onClick={frameAll} title="Frame all (H)">
          <FiMaximize />
          All
        </button>
        <button
          className={styles.btn}
          onClick={frameSelection}
          disabled={!hasSelection}
          title="Frame selection (F)"
        >
          <FiCrosshair />
          Selection
        </button>

        <div className={styles.divider} />

        <button
          className={gizmoMode === null ? styles.btnActive : styles.btn}
          onClick={() => setGizmoMode(null)}
          title="No gizmo"
        >
          <FiSlash />
        </button>
        <button
          className={gizmoMode === "translate" ? styles.btnActive : styles.btn}
          onClick={() => setGizmoMode("translate")}
          title="Move (G)"
        >
          <FiMove />
        </button>
        <button
          className={gizmoMode === "rotate" ? styles.btnActive : styles.btn}
          onClick={() => setGizmoMode("rotate")}
          title="Rotate (R)"
        >
          <FiRotateCw />
        </button>
        <button
          className={gizmoMode === "scale" ? styles.btnActive : styles.btn}
          onClick={() => setGizmoMode("scale")}
          title="Scale (T)"
        >
          <FiMaximize2 />
        </button>

        <div className={styles.divider} />

        {SHADING_BUTTONS.map((b) => (
          <button
            key={b.mode}
            className={shading === b.mode ? styles.btnActive : styles.btn}
            onClick={() => setShading(b.mode)}
            title={b.title}
          >
            {b.icon}
            {b.label}
          </button>
        ))}

        <div className={styles.divider} />

        <button
          className={styles.btn}
          onClick={() => setSpace(space === "world" ? "local" : "world")}
          title="Toggle gizmo space"
        >
          {space === "world" ? <FiGlobe /> : <FiBox />}
          {space}
        </button>
      </div>

      {legendRows.length > 0 && (
        <div className={styles.legend}>
          {legendRows.map((r) => (
            <div key={r.key} className={styles.legendRow}>
              <span
                className={styles.swatch}
                style={{ background: `#${(KIND_COLORS[r.kind] ?? 0x8b949e).toString(16).padStart(6, "0")}` }}
              />
              {r.label}
            </div>
          ))}
        </div>
      )}

      <div className={styles.hud}>
        <div className={styles.hudRow}>
          <span>
            <span className={styles.hudKey}>fps </span>
            <span className={styles.hudVal}>{hud.fps}</span>
          </span>
          <span>
            <span className={styles.hudKey}>draws </span>
            <span className={styles.hudVal}>{fmt(hud.drawCalls)}</span>
          </span>
          <span>
            <span className={styles.hudKey}>tris </span>
            <span className={styles.hudVal}>{fmt(hud.triangles)}</span>
          </span>
        </div>
        <div className={styles.hudRow}>
          <span>
            <span className={styles.hudKey}>instances </span>
            <span className={styles.hudVal}>{fmt(hud.instances)}</span>
          </span>
          <span>
            <span className={styles.hudKey}>models </span>
            <span className={styles.hudVal}>{fmt(hud.models)}</span>
          </span>
          <span>
            <span className={styles.hudKey}>entities </span>
            <span className={styles.hudVal}>{fmt(hud.entities)}</span>
          </span>
          {hud.loading > 0 && <span className={styles.hudWarn}>loading {hud.loading}</span>}
        </div>
        {shading === "textured" && (
          <div className={styles.hudRow}>
            <span
              title={`Distinct albedo textures resident, shared across every model and zone (requested at up to ${TEXTURE_MAX_SIZE}px)`}
            >
              <span className={styles.hudKey}>tex </span>
              <span className={styles.hudVal}>{fmt(hud.textures)}</span>
            </span>
            <span
              title={`Approximate GPU bytes those textures occupy, against a ${(TEXTURE_BUDGET_BYTES / (1024 * 1024 * 1024)).toFixed(2)} GB budget`}
            >
              <span className={styles.hudKey}>vram </span>
              <span
                className={hud.textureBytes > TEXTURE_BUDGET_BYTES * 0.9 ? styles.hudWarn : styles.hudVal}
              >
                {(hud.textureBytes / (1024 * 1024)).toFixed(0)} MB
              </span>
            </span>
            <span title="Model batches drawing at least one textured submesh">
              <span className={styles.hudKey}>textured </span>
              <span className={styles.hudVal}>
                {fmt(hud.texturedModels)}/{fmt(hud.models)}
              </span>
            </span>
            {hud.textureLoading > 0 && <span className={styles.hudWarn}>+{fmt(hud.textureLoading)}</span>}
            {hud.textureSkipped > 0 && (
              <span className={styles.hudWarn} title="Textures refused because the VRAM budget was spent">
                budget {fmt(hud.textureSkipped)}
              </span>
            )}
          </div>
        )}
        {hud.blockGeo > 0 && (
          <div className={styles.hudRow}>
            <span title="Scene objects whose mesh lives in the zone's own block 2 (impostor shells, projected decals) — no .model asset exists for these">
              <span className={styles.hudKey}>block-2 </span>
              <span className={hud.blockGeoLoaded < hud.blockGeo ? styles.hudWarn : styles.hudVal}>
                {fmt(hud.blockGeoLoaded)}/{fmt(hud.blockGeo)}
              </span>
            </span>
            <span title="Triangles contributed by that block-2 geometry">
              <span className={styles.hudKey}>tris </span>
              <span className={styles.hudVal}>{fmt(hud.blockGeoTriangles)}</span>
            </span>
          </div>
        )}
        {(hud.mirrored > 0 || hud.windingSuspect > 0) && (
          <div className={styles.hudRow}>
            <span title="Instances with a negative-determinant (mirrored) basis — drawn double-sided">
              <span className={styles.hudKey}>mirrored </span>
              <span className={styles.hudVal}>{fmt(hud.mirrored)}</span>
            </span>
            <span title="Dev builds only: meshes whose triangle winding disagrees with their own vertex normals. Drawn as decoded — this is a regression alarm, not a repair.">
              <span className={styles.hudKey}>mis-wound </span>
              <span className={hud.windingSuspect > 0 ? styles.hudWarn : styles.hudVal}>
                {fmt(hud.windingSuspect)}
              </span>
            </span>
          </div>
        )}
        <div className={styles.hudRow}>
          <span className={styles.hudKey}>cam</span>
          <span className={styles.hudVal}>
            {cx.toFixed(1)} {cy.toFixed(1)} {cz.toFixed(1)}
          </span>
        </div>
        <div className={styles.hudRow}>
          <span className={styles.hudKey}>{hud.hovered ? "hover" : "sel"}</span>
          <span className={styles.hudName}>
            {hud.hovered ??
              (hud.selectedCount > 1
                ? `${hud.selectedCount} objects`
                : (hud.selectedName ?? "—"))}
          </span>
        </div>
      </div>

      <div className={styles.help}>
        <div>
          <kbd>LMB</kbd> orbit / pick · <kbd>MMB</kbd>/<kbd>RMB</kbd> pan · <kbd>wheel</kbd> zoom
        </div>
        <div>
          <kbd>WASD</kbd>+<kbd>QE</kbd> fly · <kbd>shift</kbd> fast · <kbd>ctrl</kbd>+drag box-select · <kbd>N</kbd> shading
        </div>
      </div>

      {!scene && (
        <div className={styles.empty}>
          <div className={styles.emptyTitle}>No zone loaded</div>
          <div>Pick a region and a zone to view its geometry and entities.</div>
        </div>
      )}

      {marquee && (
        <div
          className={styles.marquee}
          style={{ left: marquee.x, top: marquee.y, width: marquee.w, height: marquee.h }}
        />
      )}
    </div>
  );
}
