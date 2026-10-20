import { LuCircleAlert } from "react-icons/lu";
import styles from "./StatusBar.module.css";

interface Props {
  zoneCount: number;
  /** How many regions those zones came from — see panels/RegionIndex.tsx. */
  regionCount?: number;
  /**
   * One line about what the loaded regions do to each other in world space.
   * The level format has no per-region placement, so when two regions are in
   * the view at once the bar says what was measured instead of implying they
   * belong together.
   */
  regionNote?: string | null;
  objectCount: number;
  instanceCount: number;
  entityCount: number;
  modelCount: number;
  selectionLabel: string | null;
  pendingEdits: number;
  busy: string | null;
}

export default function StatusBar({
  zoneCount,
  regionCount,
  regionNote,
  objectCount,
  instanceCount,
  entityCount,
  modelCount,
  selectionLabel,
  pendingEdits,
  busy,
}: Props) {
  return (
    <div className={styles.bar}>
      <Stat label="Zones" value={zoneCount} />
      {regionCount !== undefined && regionCount > 1 && (
        <Stat label="Regions" value={regionCount} />
      )}
      <Stat label="Objects" value={objectCount} />
      <Stat label="Model instances" value={instanceCount} />
      <Stat label="Entities" value={entityCount} />
      <Stat label="Models" value={modelCount} />

      <span className={styles.spacer} />

      {regionNote && (
        <span className={styles.dirty} title={regionNote}>
          <LuCircleAlert />
          {regionNote}
        </span>
      )}

      {selectionLabel && (
        <span className={styles.selection} title={selectionLabel}>
          {selectionLabel}
        </span>
      )}
      {busy && <span className={styles.busy}>{busy}</span>}
      {pendingEdits > 0 && (
        <span className={styles.dirty} title="Unsaved transform edits">
          <LuCircleAlert />
          {pendingEdits} pending edit{pendingEdits === 1 ? "" : "s"}
        </span>
      )}
    </div>
  );
}

function Stat({ label, value }: { label: string; value: number }) {
  return (
    <span className={styles.stat}>
      {label} <b>{value.toLocaleString()}</b>
    </span>
  );
}
