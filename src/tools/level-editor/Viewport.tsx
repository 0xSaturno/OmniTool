import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  FiBox,
  FiCrosshair,
  FiGlobe,
  FiMaximize,
  FiMaximize2,
  FiMove,
  FiRotateCw,
  FiSlash,
} from "react-icons/fi";
import { LevelRenderer, KIND_COLORS, type HudState } from "./viewport/renderer";
import { KIND_MODEL, kindName, type GizmoMode, type MeshLoader, type ZoneScene } from "./types";
import styles from "./Viewport.module.css";

export interface ViewportProps {
  scene: ZoneScene | null;
  /** Returns the RCMS buffer for a model asset id (`"0x…"`). */
  meshLoader: MeshLoader;
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
};

function fmt(n: number): string {
  return n.toLocaleString("en-US");
}

export default function Viewport(props: ViewportProps) {
  const { scene, meshLoader, selection, onSelect, onTransform, showEntities, showGeometry, wireframe } = props;

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

  useEffect(() => {
    if (!ready) return;
    const r = rendererRef.current;
    if (!r) return;
    r.setScene(scene, (id) => meshLoaderRef.current(id));
    // A rebuild drops the renderer's selection; re-apply what the shell holds.
    r.setSelection(selectionRef.current);
  }, [ready, scene]);

  // -- selection ------------------------------------------------------------

  const selectionKey = selection.join(",");
  useEffect(() => {
    if (!ready) return;
    rendererRef.current?.setSelection(selection);
    // selectionKey stands in for a deep compare of the offsets array.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [ready, selectionKey]);

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

  // -- actions --------------------------------------------------------------

  const frameAll = useCallback(() => rendererRef.current?.frameAll(), []);
  const frameSelection = useCallback(() => rendererRef.current?.frameSelection(), []);

  const kindsPresent = useMemo(() => {
    const set = new Set<number>();
    if (scene) for (const o of scene.objects) if (o.kind !== KIND_MODEL) set.add(o.kind);
    return [...set].sort((a, b) => a - b);
  }, [scene]);

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

        <button
          className={styles.btn}
          onClick={() => setSpace(space === "world" ? "local" : "world")}
          title="Toggle gizmo space"
        >
          {space === "world" ? <FiGlobe /> : <FiBox />}
          {space}
        </button>
      </div>

      {kindsPresent.length > 0 && showEntities && (
        <div className={styles.legend}>
          {kindsPresent.map((k) => (
            <div key={k} className={styles.legendRow}>
              <span
                className={styles.swatch}
                style={{ background: `#${(KIND_COLORS[k] ?? 0x8b949e).toString(16).padStart(6, "0")}` }}
              />
              {kindName(k)}
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
          <kbd>WASD</kbd>+<kbd>QE</kbd> fly · <kbd>shift</kbd> fast · <kbd>ctrl</kbd>+drag box-select
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
