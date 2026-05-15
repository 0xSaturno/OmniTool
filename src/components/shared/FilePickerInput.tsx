import { useEffect, useRef, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open, save } from "@tauri-apps/plugin-dialog";
import styles from "./FilePickerInput.module.css";

interface Props {
  label: string;
  value: string;
  onChange: (path: string) => void;
  mode: "open" | "save" | "dir";
  filters?: { name: string; extensions: string[] }[];
  placeholder?: string;
}

function normalizeDroppedPath(raw: string): string {
  const value = raw.trim();
  if (!value) return "";

  if (value.startsWith("file://")) {
    try {
      const url = new URL(value);
      let path = decodeURIComponent(url.pathname);
      if (/^\/[A-Za-z]:\//.test(path)) {
        path = path.slice(1);
      }
      return path.replace(/\//g, "\\");
    } catch {
      // Fallback below for malformed uri-list payloads.
    }
  }

  return value;
}

export default function FilePickerInput({ label, value, onChange, mode, filters, placeholder }: Props) {
  const [dragOver, setDragOver] = useState(false);
  const rowRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | null = null;

    const isInsideRow = (x: number, y: number): boolean => {
      const row = rowRef.current;
      if (!row) return false;
      const rect = row.getBoundingClientRect();
      return x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom;
    };

    getCurrentWindow()
      .onDragDropEvent((event) => {
        const payload = event.payload;

        if (payload.type === "leave") {
          setDragOver(false);
          return;
        }

        const inside = isInsideRow(payload.position.x, payload.position.y);

        if (payload.type === "enter" || payload.type === "over") {
          setDragOver(inside);
          return;
        }

        // Drop
        setDragOver(false);
        if (!inside || payload.paths.length === 0) return;

        const droppedPath = normalizeDroppedPath(payload.paths[0]);
        if (!droppedPath) return;

        console.debug("[FilePickerInput] tauri dropped path", {
          label,
          mode,
          droppedPath,
        });
        onChange(droppedPath);
      })
      .then((fn) => {
        if (disposed) {
          fn();
        } else {
          unlisten = fn;
        }
      })
      .catch((error) => {
        console.debug("[FilePickerInput] tauri drag-drop listener unavailable", {
          label,
          mode,
          error: String(error),
        });
      });

    return () => {
      disposed = true;
      if (unlisten) unlisten();
    };
  }, [label, mode, onChange]);

  async function pick() {
    if (mode === "open") {
      const result = await open({ filters, multiple: false });
      if (typeof result === "string") {
        console.debug("[FilePickerInput] picked open path", { label, mode, result });
        onChange(result);
      }
    } else if (mode === "dir") {
      const result = await open({ directory: true, multiple: false });
      if (typeof result === "string") {
        console.debug("[FilePickerInput] picked dir path", { label, mode, result });
        onChange(result);
      }
    } else {
      const result = await save({ filters });
      if (result) {
        console.debug("[FilePickerInput] picked save path", { label, mode, result });
        onChange(result);
      }
    }
  }

  return (
    <div className={styles.field}>
      <label className={styles.label}>{label}</label>
      <div
        ref={rowRef}
        className={`${styles.row} ${dragOver ? styles.rowDragOver : ""}`}
      >
        <input
          className={`${styles.input} ${dragOver ? styles.inputDragOver : ""}`}
          value={value}
          onChange={(e) => onChange(e.target.value)}
          placeholder={placeholder ?? "Drop a file here or click Browse…"}
        />
        <button className={styles.browseBtn} onClick={pick}>
          Browse
        </button>
      </div>
    </div>
  );
}
