import { useEffect, useRef } from "react";
import styles from "./StatusLog.module.css";

export type LogEntry = { type: "info" | "success" | "warning" | "error"; message: string; ts: number };

interface Props {
  entries: LogEntry[];
}

export default function StatusLog({ entries }: Props) {
  const bottomRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [entries.length]);

  if (entries.length === 0) return null;
  return (
    <div className={styles.log}>
      {entries.map((e, index) => (
        <div key={`${e.ts}-${index}`} className={`${styles.entry} ${styles[e.type]}`}>
          <span className={styles.prefix}>{prefixFor(e.type)}</span>
          {e.message}
        </div>
      ))}
      <div ref={bottomRef} />
    </div>
  );
}

function prefixFor(type: LogEntry["type"]) {
  return { info: "·", success: "✓", warning: "!", error: "✗" }[type];
}
