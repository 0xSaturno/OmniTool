import { LuCircleAlert } from "react-icons/lu";
import styles from "./StatusBar.module.css";

interface Props {
  zoneCount: number;
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
      <Stat label="Objects" value={objectCount} />
      <Stat label="Model instances" value={instanceCount} />
      <Stat label="Entities" value={entityCount} />
      <Stat label="Models" value={modelCount} />

      <span className={styles.spacer} />

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
