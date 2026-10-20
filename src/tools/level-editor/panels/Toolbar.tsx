import type { ReactNode } from "react";
import {
  LuBoxes,
  LuCrosshair,
  LuMove,
  LuRedo2,
  LuRotateCw,
  LuScaling,
  LuSparkles,
  LuTriangle,
  LuUndo2,
} from "react-icons/lu";
import styles from "./Toolbar.module.css";

/**
 * Gizmo modes the viewport's renderer understands. "No gizmo" lives in the
 * viewport's own overlay toolbar — it has no keyboard shortcut to drive.
 */
export type ToolMode = "translate" | "rotate" | "scale";

interface Props {
  mode: ToolMode;
  onMode: (m: ToolMode) => void;
  showGeometry: boolean;
  showEntities: boolean;
  wireframe: boolean;
  onToggleGeometry: () => void;
  onToggleEntities: () => void;
  onToggleWireframe: () => void;
  onFrameSelection: () => void;
  canFrame: boolean;
  canUndo: boolean;
  canRedo: boolean;
  onUndo: () => void;
  onRedo: () => void;
}

const MODES: { id: ToolMode; label: string; hint: string; icon: ReactNode }[] = [
  { id: "translate", label: "Move", hint: "Translate gizmo (G)", icon: <LuMove /> },
  { id: "rotate", label: "Rotate", hint: "Rotate gizmo (R)", icon: <LuRotateCw /> },
  { id: "scale", label: "Scale", hint: "Scale gizmo (T)", icon: <LuScaling /> },
];

export default function Toolbar({
  mode,
  onMode,
  showGeometry,
  showEntities,
  wireframe,
  onToggleGeometry,
  onToggleEntities,
  onToggleWireframe,
  onFrameSelection,
  canFrame,
  canUndo,
  canRedo,
  onUndo,
  onRedo,
}: Props) {
  return (
    <div className={styles.bar}>
      <div className={styles.group}>
        {MODES.map((m) => (
          <button
            key={m.id}
            className={`${styles.btn} ${mode === m.id ? styles.on : ""}`}
            onClick={() => onMode(m.id)}
            title={m.hint}
          >
            {m.icon}
            <span className={styles.label}>{m.label}</span>
          </button>
        ))}
      </div>

      <div className={styles.sep} />

      <div className={styles.group}>
        <button
          className={`${styles.btn} ${showGeometry ? styles.on : ""}`}
          onClick={onToggleGeometry}
          title="Show static map geometry (1)"
        >
          <LuBoxes />
          <span className={styles.label}>Geometry</span>
        </button>
        <button
          className={`${styles.btn} ${showEntities ? styles.on : ""}`}
          onClick={onToggleEntities}
          title="Show gameplay entities — actors, lights, splines, VFX (2)"
        >
          <LuSparkles />
          <span className={styles.label}>Entities</span>
        </button>
        <button
          className={`${styles.btn} ${wireframe ? styles.on : ""}`}
          onClick={onToggleWireframe}
          title="Wireframe shading (3)"
        >
          <LuTriangle />
          <span className={styles.label}>Wireframe</span>
        </button>
      </div>

      <div className={styles.sep} />

      <div className={styles.group}>
        <button
          className={styles.btn}
          onClick={onFrameSelection}
          disabled={!canFrame}
          title="Frame the selection, or the whole scene when nothing is selected (F / H)"
        >
          <LuCrosshair />
          <span className={styles.label}>Frame</span>
        </button>
      </div>

      <div className={styles.spacer} />

      <div className={styles.group}>
        <button
          className={styles.btn}
          onClick={onUndo}
          disabled={!canUndo}
          title="Undo the last transform edit (Ctrl+Z)"
        >
          <LuUndo2 />
        </button>
        <button
          className={styles.btn}
          onClick={onRedo}
          disabled={!canRedo}
          title="Redo (Ctrl+Shift+Z)"
        >
          <LuRedo2 />
        </button>
      </div>
    </div>
  );
}
