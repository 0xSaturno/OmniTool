import { useEffect, useRef, useState } from "react";
import { LuRotateCcw } from "react-icons/lu";
import type { SceneObjectDto } from "../types";
import { formatSize } from "../api";
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
  const emittedRef = useRef<number[] | null>(null);
  const offsetRef = useRef<number | null>(null);

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

  if (!object) {
    return (
      <div className={styles.panel}>
        <div className={styles.placeholder}>
          Select an object in the viewport or the scene tree to inspect it.
        </div>
        {sections.length > 0 && <SectionList sections={sections} />}
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
      <div className={styles.head}>
        <span className={styles.title}>Inspector</span>
        {edited && (
          <button className={styles.revertBtn} onClick={onRevert} title="Revert this object">
            <LuRotateCcw />
            Revert
          </button>
        )}
      </div>

      <div className={styles.card}>
        <div className={styles.row}>
          <span className={styles.key}>Kind</span>
          <span className={styles.val}>
            {object.kindName} <span className={styles.dim}>({object.kind})</span>
          </span>
        </div>
        <div className={styles.row}>
          <span className={styles.key}>Name</span>
          <span className={styles.val}>{object.name || <em className={styles.dim}>unnamed</em>}</span>
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

      {object.modelId && object.modelId !== "0x0" && (
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
          <span className={styles.groupLabel} title="The separate scale vector stored alongside the matrix in the zone record">
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
