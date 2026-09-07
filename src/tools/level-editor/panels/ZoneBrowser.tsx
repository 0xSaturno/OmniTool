import { useMemo, useState } from "react";
import {
  LuChevronDown,
  LuChevronRight,
  LuCheck,
  LuLayers,
  LuRefreshCw,
} from "react-icons/lu";
import type { LevelRegion, LevelZoneStats, ZoneEntry } from "../api";
import { KIND_LABELS, ZONE_KINDS, formatSize, zoneFileName } from "../api";
import styles from "./ZoneBrowser.module.css";

const GROUP_LABELS: Record<string, string> = {
  openarea: "Open Area",
  overlay: "Story Overlays",
  instance: "Instances",
  other: "Other",
};
const GROUP_ORDER = ["openarea", "overlay", "instance", "other"];

/** Tile groups rendered at once — the rest stay behind the search box. */
const TILE_RENDER_CAP = 150;

interface Props {
  regions: LevelRegion[];
  stats: LevelZoneStats | null;
  activeRegion: string | null;
  zones: ZoneEntry[];
  loadedPaths: Set<string>;
  busyPaths: Set<string>;
  loadingRegions: boolean;
  loadingZones: boolean;
  objectCounts: Map<string, number>;
  onSelectRegion: (key: string) => void;
  onToggleZone: (entry: ZoneEntry) => void;
  onLoadTile: (entries: ZoneEntry[]) => void;
  onClearAll: () => void;
  onRefresh: () => void;
}

export default function ZoneBrowser({
  regions,
  stats,
  activeRegion,
  zones,
  loadedPaths,
  busyPaths,
  loadingRegions,
  loadingZones,
  objectCounts,
  onSelectRegion,
  onToggleZone,
  onLoadTile,
  onClearAll,
  onRefresh,
}: Props) {
  const [filter, setFilter] = useState("");
  const [kinds, setKinds] = useState<Set<string>>(new Set());
  const [hideStubs, setHideStubs] = useState(false);
  const [collapsedGroups, setCollapsedGroups] = useState<Set<string>>(new Set());
  const [regionsOpen, setRegionsOpen] = useState(true);

  const regionGroups = useMemo(() => {
    const byGroup = new Map<string, LevelRegion[]>();
    for (const r of regions) {
      const list = byGroup.get(r.group) ?? [];
      list.push(r);
      byGroup.set(r.group, list);
    }
    for (const list of byGroup.values()) list.sort((a, b) => a.label.localeCompare(b.label));
    return GROUP_ORDER.filter((g) => byGroup.has(g)).map((g) => ({
      group: g,
      label: GROUP_LABELS[g] ?? g,
      regions: byGroup.get(g)!,
    }));
  }, [regions]);

  const tiles = useMemo(() => {
    const needle = filter.trim().toLowerCase();
    const groups = new Map<string, ZoneEntry[]>();
    for (const z of zones) {
      if (kinds.size > 0 && !kinds.has(z.kind)) continue;
      if (hideStubs && !z.hasObjects) continue;
      if (needle && !z.path.toLowerCase().includes(needle)) continue;
      const key = z.tile ?? "(no tile)";
      const list = groups.get(key) ?? [];
      list.push(z);
      groups.set(key, list);
    }
    const out = [...groups.entries()].map(([tile, entries]) => ({
      tile,
      entries: entries.sort((a, b) => a.path.localeCompare(b.path)),
      loadable: entries.filter((e) => e.hasObjects),
      loadedCount: entries.filter((e) => loadedPaths.has(e.path)).length,
    }));
    out.sort((a, b) => a.tile.localeCompare(b.tile, undefined, { numeric: true }));
    return out;
  }, [zones, kinds, hideStubs, filter, loadedPaths]);

  const shown = tiles.slice(0, TILE_RENDER_CAP);

  const toggleKind = (k: string) =>
    setKinds((prev) => {
      const next = new Set(prev);
      if (next.has(k)) next.delete(k);
      else next.add(k);
      return next;
    });

  const toggleGroup = (tile: string) =>
    setCollapsedGroups((prev) => {
      const next = new Set(prev);
      if (next.has(tile)) next.delete(tile);
      else next.add(tile);
      return next;
    });

  return (
    <div className={styles.panel}>
      <div className={styles.sectionHead}>
        <button className={styles.sectionToggle} onClick={() => setRegionsOpen((o) => !o)}>
          {regionsOpen ? <LuChevronDown /> : <LuChevronRight />}
          <span>Regions</span>
          <span className={styles.count}>{regions.length}</span>
        </button>
        <button className={styles.iconBtn} onClick={onRefresh} title="Reload the region index">
          <LuRefreshCw className={loadingRegions ? styles.spin : undefined} />
        </button>
      </div>

      {regionsOpen && (
        <div className={styles.regionList}>
          {loadingRegions && regions.length === 0 && (
            <div className={styles.hint}>Scanning archives…</div>
          )}
          {!loadingRegions && regions.length === 0 && (
            <div className={styles.hint}>No level zones found in this archive set.</div>
          )}
          {regionGroups.map((g) => (
            <div key={g.group}>
              <div className={styles.groupLabel}>{g.label}</div>
              {g.regions.map((r) => (
                <button
                  key={r.key}
                  className={`${styles.regionRow} ${activeRegion === r.key ? styles.active : ""}`}
                  onClick={() => onSelectRegion(r.key)}
                  title={r.key}
                >
                  <span className={styles.regionName}>{r.label}</span>
                  <span className={styles.count}>{r.zoneCount}</span>
                </button>
              ))}
            </div>
          ))}
        </div>
      )}

      {stats && (
        <div className={styles.statsBar}>
          <span>
            Zones <b>{stats.totalZones.toLocaleString()}</b>
          </span>
          <span>
            Regions <b>{stats.regions}</b>
          </span>
          <span>
            With objects <b>{stats.withObjects.toLocaleString()}</b>
          </span>
        </div>
      )}

      <div className={styles.sectionHead}>
        <span className={styles.sectionTitle}>
          Zones{activeRegion ? ` · ${activeRegion.split("/").pop()}` : ""}
        </span>
        {loadedPaths.size > 0 && (
          <button className={styles.linkBtn} onClick={onClearAll} title="Unload every zone">
            Clear {loadedPaths.size}
          </button>
        )}
      </div>

      <input
        className={styles.search}
        placeholder="Filter zones by path…"
        value={filter}
        onChange={(e) => setFilter(e.target.value)}
      />

      <div className={styles.chips}>
        {ZONE_KINDS.map((k) => (
          <button
            key={k}
            className={`${styles.chip} ${kinds.has(k) ? styles.chipOn : ""}`}
            onClick={() => toggleKind(k)}
            title={`Only show ${KIND_LABELS[k]} zones`}
          >
            {KIND_LABELS[k]}
          </button>
        ))}
        <button
          className={`${styles.chip} ${hideStubs ? styles.chipOn : ""}`}
          onClick={() => setHideStubs((h) => !h)}
          title="Hide zones that contain no scene objects"
        >
          Hide stubs
        </button>
      </div>

      <div className={styles.zoneList}>
        {loadingZones && <div className={styles.hint}>Loading zone list…</div>}
        {!loadingZones && !activeRegion && (
          <div className={styles.hint}>Pick a region to list its zones.</div>
        )}
        {!loadingZones && activeRegion && tiles.length === 0 && (
          <div className={styles.hint}>No zones match the filter.</div>
        )}

        {shown.map((t) => {
          const collapsed = collapsedGroups.has(t.tile);
          return (
            <div key={t.tile} className={styles.tile}>
              <div className={styles.tileHead}>
                <button className={styles.tileToggle} onClick={() => toggleGroup(t.tile)}>
                  {collapsed ? <LuChevronRight /> : <LuChevronDown />}
                  <span className={styles.tileName}>{t.tile}</span>
                  <span className={styles.count}>{t.entries.length}</span>
                </button>
                {t.loadable.length > 1 && (
                  <button
                    className={styles.tileLoadBtn}
                    onClick={() => onLoadTile(t.loadable)}
                    title={`Load all ${t.loadable.length} non-empty zones of this tile together (art + gameplay)`}
                  >
                    <LuLayers />
                    Load tile
                  </button>
                )}
              </div>

              {!collapsed &&
                t.entries.map((z) => {
                  const loaded = loadedPaths.has(z.path);
                  const busy = busyPaths.has(z.path);
                  const objs = objectCounts.get(z.path);
                  return (
                    <button
                      key={z.path}
                      className={[
                        styles.zoneRow,
                        loaded ? styles.zoneLoaded : "",
                        z.hasObjects ? "" : styles.zoneStub,
                      ].join(" ")}
                      onClick={() => onToggleZone(z)}
                      title={`${z.path}\n${z.id}\n${formatSize(z.size)}${z.hasObjects ? "" : "\nStub zone — no scene objects"}`}
                    >
                      <span className={styles.zoneCheck}>
                        {busy ? <span className={styles.dot} /> : loaded ? <LuCheck /> : null}
                      </span>
                      <span className={`${styles.kindTag} ${styles[`kind_${z.kind}`] ?? ""}`}>
                        {z.kind}
                      </span>
                      <span className={styles.zoneName}>{zoneFileName(z.path)}</span>
                      <span className={styles.zoneMeta}>
                        {objs !== undefined ? `${objs.toLocaleString()} obj` : formatSize(z.size)}
                      </span>
                    </button>
                  );
                })}
            </div>
          );
        })}

        {tiles.length > shown.length && (
          <div className={styles.hint}>
            Showing the first {shown.length} of {tiles.length} tiles — narrow the filter to see the
            rest.
          </div>
        )}
      </div>
    </div>
  );
}
