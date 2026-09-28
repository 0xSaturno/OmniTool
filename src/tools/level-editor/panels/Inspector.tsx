/**
 * THE INSPECTOR.
 *
 * Three tabs over one selection:
 *
 *   Object      what the zone record says — kind, model, actor, transform, and
 *               for a kind-1 scene object the decoded LIGHT.
 *   Properties  the selected actor's component priuses, as named, typed,
 *               editable fields (`PriusSheet`).
 *   Script      the zone's script graph (`ScriptPanel`).
 *
 * WHY THE PANEL FETCHES ITS OWN DATA. `LevelEditor.tsx` owns the zone session
 * and hands this panel a selection, not a game directory; it is not this
 * change's file. So the three commands behind the tabs are called from here,
 * against `api.currentGameDir()` (latched by the mesh loader the shell builds
 * from the same value), and every result is memoised per zone inside `api.ts`
 * so that clicking around a zone costs one round trip, not one per click.
 *
 * WHAT HAPPENS TO AN EDIT. A prius edit is planned by the backend
 * (`prius_plan_edit`), which either refuses with a reason or returns an
 * EQUAL-LENGTH byte patch. Approved patches are staged in `api.ts`. They are
 * NOT written by `level_save_zone` yet — its `ZoneEditDto` only carries
 * `{offset, matrix, scale}`, so it can rewrite a transform and nothing else.
 * The pending-edits banner says exactly that rather than letting the user
 * believe a save carried them.
 */

import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import { LuRotateCcw, LuTriangleAlert } from "react-icons/lu";
import type {
  ActorSheetsDto,
  ScriptGraphDto,
  ScriptNodeDetailDto,
  SceneObjectDto,
  StagedPriusEdit,
  TuidTarget,
  ZoneLightDto,
} from "../types";
import { KIND_LIGHT } from "../types";
import * as api from "../api";
import { formatSize } from "../api";
import PriusSheet, { type PlanEditArgs } from "./PriusSheet";
import ScriptPanel, { NODE_LIMIT } from "./ScriptPanel";
import styles from "./Inspector.module.css";

/* Structural shapes — kept local so the panel does not depend on how the
   viewport's `types.ts` happens to name these rows. */
interface ActorRow {
  index: number;
  sceneOffset: number;
  instanceId: string;
  name: string;
  assetId: string;
  assetPath: string;
}
interface SectionRow {
  tag: string;
  name: string;
  size: number;
}

export interface TransformValue {
  matrix: number[];
  scale: [number, number, number];
}

interface Props {
  object: SceneObjectDto | null;
  zonePath: string | null;
  modelPath: string | null;
  modelInstanceCount: number | null;
  actor: ActorRow | null;
  sections: SectionRow[];
  edited: boolean;
  onTransform: (next: TransformValue) => void;
  onRevert: () => void;
}

/* ------------------------------------------------------------------ */
/* Matrix helpers                                                      */
/*                                                                     */
/* The zone matrix is ROW-MAJOR with row 3 = translation, i.e. rows 0-2 */
/* are the transformed X/Y/Z basis vectors (row-vector convention).     */
/* Euler angles use the three.js 'XYZ' convention on the equivalent     */
/* column-major rotation, so decompose(compose(e)) == e.                */
/* ------------------------------------------------------------------ */

const DEG = 180 / Math.PI;
const RAD = Math.PI / 180;

export interface Trs {
  pos: [number, number, number];
  rot: [number, number, number]; // degrees
  scl: [number, number, number];
}

export function decompose(m: number[]): Trs {
  const row = (r: number): [number, number, number] => [m[r * 4], m[r * 4 + 1], m[r * 4 + 2]];
  const len = (v: [number, number, number]) => Math.hypot(v[0], v[1], v[2]);
  const r0 = row(0);
  const r1 = row(1);
  const r2 = row(2);

  // Mirrored transforms: fold the flip into X so the basis stays right-handed.
  const det =
    r0[0] * (r1[1] * r2[2] - r1[2] * r2[1]) -
    r0[1] * (r1[0] * r2[2] - r1[2] * r2[0]) +
    r0[2] * (r1[0] * r2[1] - r1[1] * r2[0]);

  let sx = len(r0) || 1;
  const sy = len(r1) || 1;
  const sz = len(r2) || 1;
  if (det < 0) sx = -sx;

  // C = transpose of the normalised 3x3 (column-major rotation).
  const c = (i: number, j: number) => m[j * 4 + i] / [sx, sy, sz][j];

  const c02 = Math.max(-1, Math.min(1, c(0, 2)));
  const y = Math.asin(c02);
  let x: number;
  let z: number;
  if (Math.abs(c02) < 0.9999999) {
    x = Math.atan2(-c(1, 2), c(2, 2));
    z = Math.atan2(-c(0, 1), c(0, 0));
  } else {
    x = Math.atan2(c(2, 1), c(1, 1));
    z = 0;
  }

  return {
    pos: [m[12], m[13], m[14]],
    rot: [x * DEG, y * DEG, z * DEG],
    scl: [sx, sy, sz],
  };
}

export function compose(t: Trs, template?: number[]): number[] {
  const [rx, ry, rz] = t.rot.map((d) => d * RAD);
  const a = Math.cos(rx);
  const b = Math.sin(rx);
  const c = Math.cos(ry);
  const d = Math.sin(ry);
  const e = Math.cos(rz);
  const f = Math.sin(rz);
  const ae = a * e;
  const af = a * f;
  const be = b * e;
  const bf = b * f;

  // Column-major rotation C (three.js XYZ order).
  const C = [
    [c * e, -c * f, d],
    [af + be * d, ae - bf * d, -b * c],
    [bf - ae * d, be + af * d, a * c],
  ];

  const out = template ? template.slice() : new Array(16).fill(0);
  for (let j = 0; j < 3; j++) {
    for (let i = 0; i < 3; i++) {
      // row-major M[j][i] = C[i][j], scaled by the row's scale
      out[j * 4 + i] = C[i][j] * t.scl[j];
    }
    out[j * 4 + 3] = template ? template[j * 4 + 3] : 0;
  }
  out[12] = t.pos[0];
  out[13] = t.pos[1];
  out[14] = t.pos[2];
  out[15] = template ? template[15] : 1;
  return out;
}

/** True for every spelling of a zero 64-bit id: `0x0`, `0x0000000000000000`, `0`. */
export function isZeroId(id: string): boolean {
  return /^(0x)?0+$/i.test(id.trim());
}

function fmt(n: number) {
  if (!Number.isFinite(n)) return "0";
  const r = Math.round(n * 10000) / 10000;
  return String(r);
}

function NumField({
  value,
  onChange,
  label,
  step = 0.1,
}: {
  value: number;
  onChange: (v: number) => void;
  label: string;
  step?: number;
}) {
  const [text, setText] = useState(() => fmt(value));
  const [focused, setFocused] = useState(false);

  useEffect(() => {
    if (!focused) setText(fmt(value));
  }, [value, focused]);

  return (
    <label className={styles.numField}>
      <span className={styles.axis}>{label}</span>
      <input
        type="number"
        step={step}
        className={styles.numInput}
        value={text}
        onFocus={() => setFocused(true)}
        onBlur={() => {
          setFocused(false);
          setText(fmt(value));
        }}
        onChange={(e) => {
          setText(e.target.value);
          const n = parseFloat(e.target.value);
          if (Number.isFinite(n)) onChange(n);
        }}
      />
    </label>
  );
}

/* ------------------------------------------------------------------ */
/* Lights                                                              */
/* ------------------------------------------------------------------ */

/**
 * Linear RGB premultiplied by intensity, as a swatch plus the numbers.
 *
 * `linearRgb` is exactly what the engine multiplies into the G-buffer, so it
 * routinely leaves 0..1 — a 40 W practical is a small colour times a large
 * intensity. The swatch therefore shows the HUE (the colour normalised so its
 * brightest channel is 1, then gamma-encoded for the screen) and the numbers
 * carry the magnitude, rather than a swatch that clips to white and says
 * nothing.
 */
function ColorSwatch({ rgb, luminance }: { rgb: readonly number[]; luminance: number }) {
  const peak = Math.max(rgb[0], rgb[1], rgb[2], 1e-6);
  const srgb = rgb.map((c) => {
    const n = Math.max(0, Math.min(1, c / peak));
    const g = n <= 0.0031308 ? n * 12.92 : 1.055 * Math.pow(n, 1 / 2.4) - 0.055;
    return Math.round(g * 255);
  });
  return (
    <span className={styles.swatchRow}>
      <span
        className={styles.swatch}
        style={{ background: `rgb(${srgb[0]}, ${srgb[1]}, ${srgb[2]})` }}
        title={`hue only — linear rgb is (${rgb.map((c) => c.toFixed(3)).join(", ")})`}
      />
      <span
        className={styles.mono}
        title="The zone stores one colour already multiplied by the light's intensity, so these linear RGB numbers ARE the intensity; the swatch above is hue only."
      >
        {rgb.map((c) => round(c, 3)).join(", ")}
        <span className={styles.dim}>
          {" "}
          linear × intensity · luminance {round(luminance, 3)}
        </span>
      </span>
    </span>
  );
}

function round(n: number, places: number) {
  const k = 10 ** places;
  return Math.round(n * k) / k;
}

function metres(n: number) {
  if (!Number.isFinite(n)) return "—";
  if (n === 0) return "0 m";
  return `${round(n, n < 10 ? 2 : 1)} m`;
}

function LightCard({ light }: { light: ZoneLightDto }) {
  const volume = light.lightType === 4 || light.lightType === 5;
  const cone = light.lightType === 1 || light.lightType === 2;
  return (
    <div className={styles.card}>
      <h4 className={styles.cardTitle}>Light</h4>

      <div className={styles.row}>
        <span className={styles.key}>Type</span>
        <span className={styles.val}>
          {light.lightTypeName} <span className={styles.dim}>({light.lightType})</span>
          {!light.active && <span className={styles.offTag}>inactive</span>}
        </span>
      </div>

      <div className={styles.row}>
        <span className={styles.key}>Colour</span>
        <ColorSwatch rgb={light.linearRgb} luminance={light.luminance} />
      </div>

      {!volume && (
        <div className={styles.row}>
          <span className={styles.key}>Radius</span>
          <span className={styles.val}>
            {metres(light.effectiveRadius)}
            {light.radiusIsDerived ? (
              <span
                className={styles.dim}
                title="no attenuation radius was authored, so the engine derives one from the colour"
              >
                {" "}
                derived from colour
              </span>
            ) : (
              <span className={styles.dim}> authored</span>
            )}
          </span>
        </div>
      )}

      {cone && (
        <div className={styles.row}>
          <span className={styles.key}>Cone</span>
          <span className={styles.val}>
            {round(light.innerConeDegrees, 1)}° inner · {round(light.outerConeDegrees, 1)}° outer
            <span className={styles.dim}> (half-angle from the axis)</span>
          </span>
        </div>
      )}

      {volume && (
        <div className={styles.row}>
          <span className={styles.key}>Extents</span>
          <span className={styles.val}>
            {light.extents.map((e) => round(e, 2)).join(" × ")} m half-extents,{" "}
            {light.volumeIsEllipsoid ? "ellipsoid" : "box"}
          </span>
        </div>
      )}

      <div className={styles.row}>
        <span className={styles.key}>Fade</span>
        <span className={styles.val}>
          out at {metres(light.fadeOutDist)}
          {light.nearFadeStartDist > 0 && <> · near from {metres(light.nearFadeStartDist)}</>}
          {light.cutOffDist > 0 && (
            <>
              {" "}
              · cut {metres(light.cutOnDist)}–{metres(light.cutOffDist)}
            </>
          )}
        </span>
      </div>

      <div className={styles.row}>
        <span className={styles.key}>Shadows</span>
        <span className={styles.val}>
          {light.castsShadows ? (
            <>
              {light.shadowCastTypeName} · {light.shadowResName}
              {light.shadowFadeOutDist > 0 && <> · out at {metres(light.shadowFadeOutDist)}</>}
            </>
          ) : (
            <span className={styles.dim}>none</span>
          )}
        </span>
      </div>

      <div className={styles.row}>
        <span className={styles.key}>Specular</span>
        <span className={styles.val}>
          {light.specTypeName} <span className={styles.dim}>× {round(light.specIntensity, 3)}</span>
        </span>
      </div>

      {light.volumetricFogIntensity > 0 && (
        <div className={styles.row}>
          <span className={styles.key}>Volumetric</span>
          <span className={styles.val}>{round(light.volumetricFogIntensity, 3)}</span>
        </div>
      )}

      {(light.bulbRadius > 0 || light.bulbLength > 0) && (
        <div className={styles.row}>
          <span className={styles.key}>Bulb</span>
          <span className={styles.val}>
            {light.bulbIsRectangle ? "rectangle" : "capsule"} · r {metres(light.bulbRadius)} · len{" "}
            {metres(light.bulbLength)}
          </span>
        </div>
      )}

      <div className={styles.row}>
        <span className={styles.key}>Conditions</span>
        <span className={styles.val}>
          {light.lightingConditionNames.join(", ") || "none"}
          {light.lightingConditions !== 0x1f && (
            <span className={styles.dim}> (excluded under the rest)</span>
          )}
        </span>
      </div>

      {light.flagNames.length > 0 && (
        <div className={styles.row}>
          <span className={styles.key}>Flags</span>
          <span className={styles.chips}>
            {light.flagNames.map((f) => (
              <span key={f} className={styles.chip}>
                {f}
              </span>
            ))}
          </span>
        </div>
      )}

      {light.assetPath && (
        <div className={styles.path} title="inline gobo / projected material / IES path">
          {light.assetPath}
        </div>
      )}
    </div>
  );
}

/* ------------------------------------------------------------------ */
/* Data hooks                                                          */
/* ------------------------------------------------------------------ */

/** `useState` + an always-latest guard, so a slow reply cannot overwrite a new one. */
function useLatest<T>(
  run: (() => Promise<T>) | null,
  deps: unknown[],
): { data: T | null; loading: boolean; error: string | null; reload: () => void } {
  const [state, setState] = useState<{ data: T | null; loading: boolean; error: string | null }>({
    data: null,
    loading: false,
    error: null,
  });
  const [nonce, setNonce] = useState(0);
  const seq = useRef(0);

  useEffect(() => {
    if (!run) {
      setState({ data: null, loading: false, error: null });
      return;
    }
    const mine = ++seq.current;
    setState((s) => ({ data: s.data, loading: true, error: null }));
    run().then(
      (data) => {
        if (seq.current === mine) setState({ data, loading: false, error: null });
      },
      (e) => {
        if (seq.current === mine) setState({ data: null, loading: false, error: String(e) });
      },
    );
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [...deps, nonce]);

  return { ...state, reload: () => setNonce((n) => n + 1) };
}

type Tab = "object" | "props" | "script";

/* ------------------------------------------------------------------ */
/* The panel                                                           */
/* ------------------------------------------------------------------ */

export default function Inspector({
  object,
  zonePath,
  modelPath,
  modelInstanceCount,
  actor,
  sections,
  edited,
  onTransform,
  onRevert,
}: Props) {
  const [trs, setTrs] = useState<Trs | null>(null);
  const [zoneScale, setZoneScale] = useState<[number, number, number]>([1, 1, 1]);
  const [showRaw, setShowRaw] = useState(false);
  const [tab, setTab] = useState<Tab>("object");
  const emittedRef = useRef<number[] | null>(null);
  const offsetRef = useRef<number | null>(null);

  const gameDir = api.currentGameDir();

  /* ---- staged prius edits ---- */
  const staged = useSyncExternalStore(api.subscribeStagedPriusEdits, api.stagedPriusEditList);
  const stagedHere = useMemo(
    () => (zonePath ? staged.filter((e) => e.zonePath === zonePath) : staged),
    [staged, zonePath],
  );

  /* ---- properties tab ---- */
  // A tuid link can pin the sheet to a different actor of the same zone.
  const [pinnedActor, setPinnedActor] = useState<TuidTarget | null>(null);
  useEffect(() => setPinnedActor(null), [actor?.index, zonePath]);

  const sheetActorIndex = pinnedActor?.actorIndex ?? actor?.index ?? null;
  const sheets = useLatest<ActorSheetsDto>(
    gameDir && zonePath && sheetActorIndex !== null && tab === "props"
      ? () => api.priusActorSheet(gameDir, zonePath, sheetActorIndex)
      : null,
    [gameDir, zonePath, sheetActorIndex, tab],
  );

  const tuids = useLatest<Map<string, TuidTarget>>(
    gameDir && zonePath && tab === "props" ? () => api.zoneTuidIndex(gameDir, zonePath) : null,
    [gameDir, zonePath, tab],
  );

  const planEdit = useCallback(
    async (args: PlanEditArgs) => {
      if (!gameDir || !zonePath) throw new Error("no zone loaded");
      const plan = await api.planPriusEdit({
        gameDir,
        zonePath,
        row: args.row,
        fieldPath: args.fieldPath,
        number: args.number,
        text: args.text,
        scriptAction: args.scriptAction,
      });
      if (plan.ok && plan.bytes && plan.oldBytes && plan.sectionTag && plan.sectionOffset !== null) {
        api.stagePriusEdit({
          zonePath,
          row: args.row,
          scriptAction: args.scriptAction,
          component: sheets.data?.sheets.find((s) => s.row === args.row)?.view.component ?? "",
          fieldPath: args.fieldPath,
          sectionTag: plan.sectionTag,
          sectionOffset: plan.sectionOffset,
          bytes: plan.bytes,
          oldBytes: plan.oldBytes,
          oldText: plan.oldText ?? "",
          newText: plan.newText ?? "",
        });
      }
      return plan;
    },
    [gameDir, zonePath, sheets.data],
  );

  const unstage = useCallback((e: StagedPriusEdit) => api.unstagePriusEdit(e), []);

  /* ---- light ---- */
  const lighting = useLatest(
    gameDir && zonePath && object?.kind === KIND_LIGHT
      ? () => api.zoneLightingCached(gameDir, zonePath)
      : null,
    [gameDir, zonePath, object?.kind === KIND_LIGHT ? object.instanceId : null],
  );
  const light = useMemo(() => {
    if (!lighting.data || !object) return null;
    const key = api.normaliseTuid(object.instanceId);
    return lighting.data.lights.find((l) => l.tuid === key) ?? null;
  }, [lighting.data, object]);

  /* ---- script tab ---- */
  const [subgraph, setSubgraph] = useState<number | null>(null);
  const [node, setNode] = useState<number | null>(null);
  useEffect(() => {
    setSubgraph(null);
    setNode(null);
  }, [zonePath]);

  const graph = useLatest<ScriptGraphDto>(
    gameDir && zonePath && tab === "script"
      ? () => api.zoneScriptGraph(gameDir, zonePath, subgraph ?? undefined, NODE_LIMIT)
      : null,
    [gameDir, zonePath, subgraph, tab],
  );
  const nodeDetail = useLatest<ScriptNodeDetailDto>(
    gameDir && zonePath && node !== null && tab === "script"
      ? () => api.zoneScriptNode(gameDir, zonePath, node)
      : null,
    [gameDir, zonePath, node, tab],
  );
  const nodePrius = useLatest(
    gameDir && zonePath && node !== null && tab === "script"
      ? () => api.priusScriptSheet(gameDir, zonePath, node)
      : null,
    [gameDir, zonePath, node, tab],
  );

  // Re-seed the editable fields whenever a different object is selected, or
  // when the matrix changed somewhere else (viewport gizmo, undo, revert).
  useEffect(() => {
    if (!object) {
      setTrs(null);
      offsetRef.current = null;
      return;
    }
    const changedObject = offsetRef.current !== object.offset;
    if (!changedObject && emittedRef.current === object.matrix) return;
    offsetRef.current = object.offset;
    emittedRef.current = object.matrix;
    setTrs(decompose(object.matrix));
    setZoneScale([object.scale[0], object.scale[1], object.scale[2]]);
  }, [object]);

  const tabs = (
    <div className={styles.tabs}>
      {(
        [
          ["object", "Object"],
          ["props", actor ? "Properties" : "Properties"],
          ["script", "Script"],
        ] as [Tab, string][]
      ).map(([id, label]) => (
        <button
          key={id}
          type="button"
          className={`${styles.tab} ${tab === id ? styles.tabOn : ""}`}
          onClick={() => setTab(id)}
        >
          {label}
          {id === "props" && stagedHere.length > 0 && (
            <span className={styles.tabBadge}>{stagedHere.length}</span>
          )}
        </button>
      ))}
    </div>
  );

  const head = (
    <div className={styles.head}>
      <span className={styles.title}>Inspector</span>
      {edited && (
        <button className={styles.revertBtn} onClick={onRevert} title="Revert this object">
          <LuRotateCcw />
          Revert
        </button>
      )}
    </div>
  );

  if (!object) {
    return (
      <div className={styles.panel}>
        {head}
        {zonePath && tabs}
        {zonePath && tab === "script" ? (
          <ScriptPanel
            graph={graph.data}
            loading={graph.loading}
            error={graph.error}
            subgraph={subgraph}
            onSubgraph={setSubgraph}
            selectedNode={node}
            onSelectNode={setNode}
            detail={nodeDetail.data}
            detailLoading={nodeDetail.loading}
            priusSlot={
              nodePrius.data ? (
                <PriusSheet scriptSheet={nodePrius.data} sheets={null} zonePath={zonePath} />
              ) : null
            }
          />
        ) : (
          <>
            <div className={styles.placeholder}>
              Select an object in the viewport or the scene tree to inspect it.
            </div>
            {sections.length > 0 && <SectionList sections={sections} />}
          </>
        )}
      </div>
    );
  }

  const push = (next: Trs) => {
    setTrs(next);
    const matrix = compose(next, object.matrix);
    emittedRef.current = matrix;
    onTransform({ matrix, scale: zoneScale });
  };

  const pushZoneScale = (next: [number, number, number]) => {
    setZoneScale(next);
    onTransform({ matrix: object.matrix, scale: next });
  };

  const t = trs ?? decompose(object.matrix);

  return (
    <div className={styles.panel}>
      {head}
      {tabs}

      {tab === "object" && (
        <>
          <div className={styles.card}>
            <div className={styles.row}>
              <span className={styles.key}>Kind</span>
              <span className={styles.val}>
                {object.kindName} <span className={styles.dim}>({object.kind})</span>
              </span>
            </div>
            <div className={styles.row}>
              <span className={styles.key}>Name</span>
              <span className={styles.val}>
                {object.name || <em className={styles.dim}>unnamed</em>}
              </span>
            </div>
            <div className={styles.row}>
              <span className={styles.key}>Instance</span>
              <span className={styles.mono}>{object.instanceId}</span>
            </div>
            <div className={styles.row}>
              <span className={styles.key}>Offset</span>
              <span className={styles.mono}>0x{object.offset.toString(16)}</span>
            </div>
            <div className={styles.row}>
              <span className={styles.key}>Size</span>
              <span className={styles.mono}>{object.size} B</span>
            </div>
            <div className={styles.row}>
              <span className={styles.key}>Flags</span>
              <span className={styles.mono}>
                0x{(object.flags >>> 0).toString(16).padStart(8, "0")}{" "}
                <span className={styles.dim}>({object.flags >>> 0})</span>
              </span>
            </div>
            {zonePath && (
              <div className={styles.row}>
                <span className={styles.key}>Zone</span>
                <span className={styles.path}>{zonePath}</span>
              </div>
            )}
          </div>

          {object.kind === KIND_LIGHT && lighting.loading && (
            <div className={styles.loading}>decoding this zone&apos;s lights…</div>
          )}
          {object.kind === KIND_LIGHT && light && <LightCard light={light} />}
          {object.kind === KIND_LIGHT && !light && !lighting.loading && (
            <div className={styles.loading}>
              {lighting.error ?? "this light is not in Zone Scene Objects for this zone"}
            </div>
          )}

          {/* `level_load_zone` formats the model id as a padded 16-digit hex
              string, so "no model" arrives as 0x0000000000000000 and a bare
              !== "0x0" test showed an empty Model card on every volume, light
              and trigger in the zone. Test the value, not the spelling. */}
          {object.modelId && !isZeroId(object.modelId) && (
            <div className={styles.card}>
              <h4 className={styles.cardTitle}>Model</h4>
              <div className={styles.path}>{modelPath || object.modelId}</div>
              <div className={styles.row}>
                <span className={styles.key}>Model ID</span>
                <span className={styles.mono}>{object.modelId}</span>
              </div>
              <div className={styles.row}>
                <span className={styles.key}>Index</span>
                <span className={styles.mono}>{object.modelIndex}</span>
              </div>
              {modelInstanceCount !== null && (
                <div className={styles.row}>
                  <span className={styles.key}>Instances</span>
                  <span className={styles.mono}>{modelInstanceCount.toLocaleString()}</span>
                </div>
              )}
            </div>
          )}

          {actor && (
            <div className={styles.card}>
              <h4 className={styles.cardTitle}>Actor</h4>
              <div className={styles.path}>{actor.assetPath || actor.assetId}</div>
              <div className={styles.row}>
                <span className={styles.key}>Actor #</span>
                <span className={styles.mono}>{actor.index}</span>
              </div>
              <div className={styles.row}>
                <span className={styles.key}>Asset ID</span>
                <span className={styles.mono}>{actor.assetId}</span>
              </div>
              <button className={styles.rawToggle} onClick={() => setTab("props")}>
                Show its property sheet →
              </button>
            </div>
          )}

          <div className={styles.card}>
            <h4 className={styles.cardTitle}>Transform</h4>

            <div className={styles.group}>
              <span className={styles.groupLabel}>Position</span>
              <div className={styles.fields}>
                {(["X", "Y", "Z"] as const).map((ax, i) => (
                  <NumField
                    key={ax}
                    label={ax}
                    value={t.pos[i]}
                    step={0.25}
                    onChange={(v) => {
                      const pos = [...t.pos] as [number, number, number];
                      pos[i] = v;
                      push({ ...t, pos });
                    }}
                  />
                ))}
              </div>
            </div>

            <div className={styles.group}>
              <span className={styles.groupLabel}>Rotation (deg)</span>
              <div className={styles.fields}>
                {(["X", "Y", "Z"] as const).map((ax, i) => (
                  <NumField
                    key={ax}
                    label={ax}
                    value={t.rot[i]}
                    step={1}
                    onChange={(v) => {
                      const rot = [...t.rot] as [number, number, number];
                      rot[i] = v;
                      push({ ...t, rot });
                    }}
                  />
                ))}
              </div>
            </div>

            <div className={styles.group}>
              <span className={styles.groupLabel}>Scale (matrix)</span>
              <div className={styles.fields}>
                {(["X", "Y", "Z"] as const).map((ax, i) => (
                  <NumField
                    key={ax}
                    label={ax}
                    value={t.scl[i]}
                    step={0.05}
                    onChange={(v) => {
                      const scl = [...t.scl] as [number, number, number];
                      scl[i] = v === 0 ? 1e-4 : v;
                      push({ ...t, scl });
                    }}
                  />
                ))}
              </div>
            </div>

            <div className={styles.group}>
              <span
                className={styles.groupLabel}
                title="The separate scale vector stored alongside the matrix in the zone record"
              >
                Zone scale vector
              </span>
              <div className={styles.fields}>
                {(["X", "Y", "Z"] as const).map((ax, i) => (
                  <NumField
                    key={ax}
                    label={ax}
                    value={zoneScale[i]}
                    step={0.05}
                    onChange={(v) => {
                      const next = [...zoneScale] as [number, number, number];
                      next[i] = v;
                      pushZoneScale(next);
                    }}
                  />
                ))}
              </div>
            </div>

            <button className={styles.rawToggle} onClick={() => setShowRaw((s) => !s)}>
              {showRaw ? "Hide" : "Show"} raw matrix
            </button>
            {showRaw && (
              <div className={styles.matrix}>
                {[0, 1, 2, 3].map((r) => (
                  <div key={r} className={styles.matrixRow}>
                    {[0, 1, 2, 3].map((c) => (
                      <span key={c}>{fmt(object.matrix[r * 4 + c])}</span>
                    ))}
                  </div>
                ))}
              </div>
            )}
          </div>

          {sections.length > 0 && <SectionList sections={sections} />}
        </>
      )}

      {tab === "props" && (
        <>
          {stagedHere.length > 0 && <PendingBanner edits={stagedHere} zonePath={zonePath} />}

          {pinnedActor && (
            <div className={styles.breadcrumb}>
              following a tuid to <strong>{pinnedActor.name}</strong>
              <button type="button" className={styles.linkBtn} onClick={() => setPinnedActor(null)}>
                back to the selection
              </button>
            </div>
          )}

          {sheetActorIndex === null ? (
            <div className={styles.placeholder}>
              This scene object is not an actor row, so it carries no component priuses. Select an
              actor to see its property sheet.
            </div>
          ) : (
            <>
              {sheets.data && (
                <div className={styles.card}>
                  <div className={styles.path}>{sheets.data.actorAsset || sheets.data.uid}</div>
                  <div className={styles.row}>
                    <span className={styles.key}>Actor</span>
                    <span className={styles.val}>
                      {sheets.data.actorName || <em className={styles.dim}>unnamed</em>}{" "}
                      <span className={styles.dim}>#{sheets.data.actorIndex}</span>
                    </span>
                  </div>
                  <div className={styles.row}>
                    <span className={styles.key}>Flags</span>
                    <span className={styles.chips}>
                      {sheets.data.isTemplate && <span className={styles.chip}>template</span>}
                      {sheets.data.startsActive && <span className={styles.chip}>starts active</span>}
                      {sheets.data.flagNames.map((f) => (
                        <span key={f} className={styles.chip}>
                          {f}
                        </span>
                      ))}
                    </span>
                  </div>
                  <div className={styles.row}>
                    <span className={styles.key}>Priuses</span>
                    <span className={styles.mono}>{sheets.data.sheets.length}</span>
                  </div>
                </div>
              )}
              <PriusSheet
                sheets={sheets.data}
                zonePath={zonePath}
                loading={sheets.loading}
                error={sheets.error}
                tuids={tuids.data}
                staged={stagedHere}
                onPlanEdit={planEdit}
                onUnstage={unstage}
                onSelectTuid={(target) => {
                  if (target.actorIndex !== undefined) setPinnedActor(target);
                }}
              />
            </>
          )}
        </>
      )}

      {tab === "script" && (
        <ScriptPanel
          graph={graph.data}
          loading={graph.loading}
          error={graph.error}
          subgraph={subgraph}
          onSubgraph={setSubgraph}
          selectedNode={node}
          onSelectNode={setNode}
          detail={nodeDetail.data}
          detailLoading={nodeDetail.loading}
          priusSlot={
            nodePrius.data ? (
              <PriusSheet scriptSheet={nodePrius.data} sheets={null} zonePath={zonePath} />
            ) : null
          }
        />
      )}
    </div>
  );
}

/**
 * What is staged, and the honest truth about what a save will do with it.
 *
 * `level_save_zone` takes `ZoneEditDto { offset, matrix, scale }` — a scene
 * object's transform and nothing else. A prius patch is a byte range in a
 * different DAT1 section, so it cannot ride that call. Saying so here is the
 * only alternative to dropping the edit or implying it was written.
 */
function PendingBanner({ edits, zonePath }: { edits: StagedPriusEdit[]; zonePath: string | null }) {
  const [open, setOpen] = useState(false);
  return (
    <div className={styles.pending}>
      <div className={styles.pendingHead}>
        <LuTriangleAlert />
        <span>
          {edits.length} prius edit{edits.length === 1 ? "" : "s"} staged and validated
        </span>
        <button type="button" className={styles.linkBtn} onClick={() => setOpen((o) => !o)}>
          {open ? "hide" : "details"}
        </button>
        <button
          type="button"
          className={styles.linkBtn}
          onClick={() => api.clearStagedPriusEdits(zonePath ?? undefined)}
        >
          discard
        </button>
      </div>
      <div className={styles.pendingNote}>
        The backend approved an equal-length byte patch for each. They are NOT written by Save yet:
        `level_save_zone` only carries scene-object transforms, not section byte patches.
      </div>
      {open && (
        <div className={styles.pendingList}>
          {edits.map((e, i) => (
            <div key={i} className={styles.pendingRow}>
              <span className={styles.pendingPath}>
                {e.component ? `${e.component}.` : ""}
                {e.fieldPath}
              </span>
              <span className={styles.mono}>
                {e.oldText} → {e.newText}
              </span>
              <span className={styles.dim}>
                {e.sectionTag}+0x{e.sectionOffset.toString(16)} · {e.bytes.length} B
              </span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

function SectionList({ sections }: { sections: SectionRow[] }) {
  return (
    <div className={styles.card}>
      <h4 className={styles.cardTitle}>Zone sections ({sections.length})</h4>
      <div className={styles.sections}>
        {sections.map((s, i) => (
          <div key={`${s.tag}-${i}`} className={styles.sectionRow} title={s.name}>
            <span className={styles.sectionTag}>{s.tag}</span>
            <span className={styles.sectionName}>{s.name}</span>
            <span className={styles.sectionSize}>{formatSize(s.size)}</span>
          </div>
        ))}
      </div>
    </div>
  );
}
