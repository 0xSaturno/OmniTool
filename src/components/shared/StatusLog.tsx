import { useEffect, useRef, useState } from "react";
import styles from "./StatusLog.module.css";
import { LuChevronUp, LuChevronDown } from "react-icons/lu";

export type LogEntry = { type: "info" | "success" | "warning" | "error"; message: string; ts: number };

interface Props {
  entries: LogEntry[];
}

export default function StatusLog({ entries }: Props) {
  const bottomRef = useRef<HTMLDivElement>(null);
  const [collapsed, setCollapsed] = useState(true);

  useEffect(() => {
    if (!collapsed) {
      bottomRef.current?.scrollIntoView({ behavior: "smooth" });
    }
  }, [entries.length, collapsed]);

  if (entries.length === 0) return null;

  const lastEntry = entries[entries.length - 1];

  if (collapsed) {
    return (
      <div className={styles.pillContainer}>
        <button 
          className={`${styles.pill} ${styles[`pill-${lastEntry.type}`]}`}
          onClick={() => setCollapsed(false)}
          title="Expand Log"
        >
          <span className={styles.pillPrefix}>{prefixFor(lastEntry.type)}</span>
          <span className={styles.pillMessage}>{lastEntry.message}</span>
          <LuChevronUp className={styles.pillIcon} />
        </button>
      </div>
    );
  }

  return (
    <div className={styles.log}>
      <div className={styles.logHeader} onClick={() => setCollapsed(true)} title="Collapse Log" role="button">
        <span className={styles.logTitle}>Status Log</span>
        <div className={styles.collapseIcon}>
          <LuChevronDown />
        </div>
      </div>
      <div className={styles.logEntries}>
        {entries.map((e, index) => (
          <div key={`${e.ts}-${index}`} className={`${styles.entry} ${styles[e.type]}`}>
            <span className={styles.prefix}>{prefixFor(e.type)}</span>
            {e.message}
          </div>
        ))}
        <div ref={bottomRef} />
      </div>
    </div>
  );
}

function prefixFor(type: LogEntry["type"]) {
  return { info: "·", success: "✓", warning: "!", error: "✗" }[type];
}
