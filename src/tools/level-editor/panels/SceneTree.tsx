import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { LuChevronDown, LuChevronRight, LuPencil } from "react-icons/lu";
import type { SceneObjectDto } from "../types";
import styles from "./SceneTree.module.css";

/**
 * Scene tree, grouped by object kind and (for model instances) by model.
 *
 * The list is windowed — only the rows that fit the scroll viewport plus a
 * small overscan are mounted — so a zone with 5,000+ objects (or a whole tile
 * with several zones loaded) stays responsive. Nothing is capped or dropped.
 */

const ROW_H = 20;
const OVERSCAN = 12;

type Row =
  | { kind: "group"; id: string; label: string; count: number; depth: number; sub?: string }
  | { kind: "object"; id: string; obj: SceneObjectDto; depth: number };

interface Props {
  objects: SceneObjectDto[];
  modelPaths: Map<string, string>;
  selected: number | null;
  onSelect: (offset: number | null) => void;
  editedOffsets: Set<number>;
  zoneOf?: (offset: number) => string | undefined;
}

function shortModel(path: string, id: string) {
  if (!path) return id;
  const file = path.split("/").pop() ?? path;
  return file.replace(/\.model$/i, "");
}

export default function SceneTree({
  objects,
  modelPaths,
  selected,
  onSelect,
  editedOffsets,
  zoneOf,
}: Props) {
  const [filter, setFilter] = useState("");
  const [collapsed, setCollapsed] = useState<Set<string>>(() => new Set());
  const [scrollTop, setScrollTop] = useState(0);
  const [viewH, setViewH] = useState(400);
  const scrollRef = useRef<HTMLDivElement>(null);

  const needle = filter.trim().toLowerCase();

  // ---- grouping -----------------------------------------------------------
  const groups = useMemo(() => {
    const byKind = new Map<
      string,
      { kindName: string; kind: number; objects: SceneObjectDto[]; models: Map<string, SceneObjectDto[]> }
    >();

    for (const o of objects) {
      if (needle) {
        const modelPath = o.modelId ? modelPaths.get(o.modelId) ?? "" : "";
        const hay = `${o.name} ${o.kindName} ${modelPath}`.toLowerCase();
        if (!hay.includes(needle)) continue;
      }
      let g = byKind.get(o.kindName);
      if (!g) {
        g = { kindName: o.kindName, kind: o.kind, objects: [], models: new Map() };
        byKind.set(o.kindName, g);
      }
      if (o.kind === 0 && o.modelId && o.modelId !== "0x0") {
        const list = g.models.get(o.modelId) ?? [];
        list.push(o);
        g.models.set(o.modelId, list);
      } else {
        g.objects.push(o);
      }
    }

    const out = [...byKind.values()];
    out.sort((a, b) => a.kind - b.kind);
    for (const g of out) {
      g.objects.sort((a, b) => a.offset - b.offset);
    }
    return out;
  }, [objects, modelPaths, needle]);

  const total = useMemo(
    () => groups.reduce((n, g) => n + g.objects.length + [...g.models.values()].reduce((m, l) => m + l.length, 0), 0),
    [groups],
  );

  // ---- flatten to rows ----------------------------------------------------
  const rows = useMemo(() => {
    const out: Row[] = [];
    for (const g of groups) {
      const gid = `k:${g.kindName}`;
      const modelLists = [...g.models.entries()].sort((a, b) => b[1].length - a[1].length);
      const count = g.objects.length + modelLists.reduce((n, [, l]) => n + l.length, 0);
      out.push({ kind: "group", id: gid, label: g.kindName, count, depth: 0 });
      if (collapsed.has(gid)) continue;

      for (const [modelId, list] of modelLists) {
        const mid = `${gid}/m:${modelId}`;
        const path = modelPaths.get(modelId) ?? "";
        out.push({
          kind: "group",
          id: mid,
          label: shortModel(path, modelId),
          sub: path || modelId,
          count: list.length,
          depth: 1,
        });
        if (collapsed.has(mid)) continue;
        for (const o of list) out.push({ kind: "object", id: `o:${o.offset}`, obj: o, depth: 2 });
      }
      for (const o of g.objects) {
        out.push({ kind: "object", id: `o:${o.offset}`, obj: o, depth: 1 });
      }
    }
    return out;
  }, [groups, collapsed, modelPaths]);

  // Collapse everything the first time a scene arrives so the tree opens tidy.
  const seededRef = useRef(false);
  useEffect(() => {
    if (seededRef.current || groups.length === 0) return;
    seededRef.current = true;
    const next = new Set<string>();
    for (const g of groups) {
      for (const modelId of g.models.keys()) next.add(`k:${g.kindName}/m:${modelId}`);
    }
    setCollapsed(next);
  }, [groups]);

  // ---- keep the selection visible -----------------------------------------
  useEffect(() => {
    if (selected === null) return;
    // Expand the ancestors of the selected object if they are collapsed.
    const target = objects.find((o) => o.offset === selected);
    if (!target) return;
    const gid = `k:${target.kindName}`;
    const mid = target.kind === 0 && target.modelId ? `${gid}/m:${target.modelId}` : null;
    setCollapsed((prev) => {
      if (!prev.has(gid) && (!mid || !prev.has(mid))) return prev;
      const next = new Set(prev);
      next.delete(gid);
      if (mid) next.delete(mid);
      return next;
    });
  }, [selected, objects]);

  useLayoutEffect(() => {
    if (selected === null) return;
    const idx = rows.findIndex((r) => r.kind === "object" && r.obj.offset === selected);
    if (idx < 0) return;
    const el = scrollRef.current;
    if (!el) return;
    const top = idx * ROW_H;
    if (top < el.scrollTop || top + ROW_H > el.scrollTop + el.clientHeight) {
      el.scrollTop = Math.max(0, top - el.clientHeight / 2);
    }
  }, [selected, rows]);

  // ---- viewport measurement ----------------------------------------------
  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const update = () => setViewH(el.clientHeight);
    update();
    const ro = new ResizeObserver(update);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  const onScroll = useCallback((e: React.UIEvent<HTMLDivElement>) => {
    setScrollTop(e.currentTarget.scrollTop);
  }, []);

  const start = Math.max(0, Math.floor(scrollTop / ROW_H) - OVERSCAN);
  const end = Math.min(rows.length, Math.ceil((scrollTop + viewH) / ROW_H) + OVERSCAN);
  const slice = rows.slice(start, end);

  const toggle = (id: string) =>
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });

  return (
    <div className={styles.panel}>
      <div className={styles.head}>
        <span className={styles.title}>Scene Tree</span>
        <span className={styles.count}>{total.toLocaleString()}</span>
      </div>
      <input
        className={styles.search}
        placeholder="Filter objects…"
        value={filter}
        onChange={(e) => setFilter(e.target.value)}
      />
      <div className={styles.scroll} ref={scrollRef} onScroll={onScroll}>
        {rows.length === 0 ? (
          <div className={styles.hint}>
            {objects.length === 0 ? "No zone loaded." : "No objects match the filter."}
          </div>
        ) : (
          <div style={{ height: rows.length * ROW_H, position: "relative" }}>
            <div style={{ position: "absolute", top: start * ROW_H, left: 0, right: 0 }}>
              {slice.map((r) =>
                r.kind === "group" ? (
                  <button
                    key={r.id}
                    className={styles.groupRow}
                    style={{ paddingLeft: `${0.25 + r.depth * 0.75}rem`, height: ROW_H }}
                    onClick={() => toggle(r.id)}
                    title={r.sub}
                  >
                    <span className={styles.chev}>
                      {collapsed.has(r.id) ? <LuChevronRight /> : <LuChevronDown />}
                    </span>
                    <span className={styles.groupLabel}>{r.label}</span>
                    <span className={styles.count}>{r.count.toLocaleString()}</span>
                  </button>
                ) : (
                  <button
                    key={r.id}
                    className={`${styles.objRow} ${selected === r.obj.offset ? styles.selected : ""}`}
                    style={{ paddingLeft: `${0.55 + r.depth * 0.75}rem`, height: ROW_H }}
                    onClick={() => onSelect(r.obj.offset)}
                    title={`${r.obj.kindName} · ${r.obj.instanceId}${
                      zoneOf ? `\n${zoneOf(r.obj.offset) ?? ""}` : ""
                    }`}
                  >
                    <span className={styles.objName}>
                      {r.obj.name || `${r.obj.kindName} @${r.obj.offset.toString(16)}`}
                    </span>
                    {editedOffsets.has(r.obj.offset) && (
                      <LuPencil className={styles.editedIcon} title="Edited" />
                    )}
                  </button>
                ),
              )}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
