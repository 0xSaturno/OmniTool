import { useMemo, useState } from "react";
import {
  LuChevronDown,
  LuChevronRight,
  LuCheck,
  LuLayers,
  LuLayers3,
  LuMoveHorizontal,
  LuRefreshCw,
  LuSearch,
  LuTriangleAlert,
  LuX,
} from "react-icons/lu";
import type { LevelRegion, LevelZoneStats, ZoneEntry } from "../api";
import { KIND_LABELS, ZONE_KINDS, formatSize, zoneFileName } from "../api";
import {
  relationSentence,
  searchRegions,
  searchZones,
  siblingRegions,
  type RegionLoadSummary,
  type RegionMixReport,
} from "./RegionIndex";
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

/** Progress of a running "Load all" run, driven by the shell. */
export interface BulkLoadProgress {
  done: number;
  total: number;
  failed: number;
}

/** Zone counts for one kind, over the zones the search box and "Hide stubs" leave listed. */
interface KindCount {
  /** Rows listed for this kind, stubs included. */
  listed: number;
  /** Of those, the ones that actually carry scene objects. */
  loadable: number;
  /** Of those, the ones not loaded yet — what a click would queue. */
  pending: number;
}

const EMPTY_COUNT: KindCount = { listed: 0, loadable: 0, pending: 0 };

/** How many zone rows the cross-group find box will list at once. */
const FIND_ZONE_LIMIT = 40;

/** How many regions one "list their zones" click is allowed to fetch. */
const PREFETCH_LIMIT = 8;

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
  /**
   * Zone lists already fetched, by region key. The cross-group find box can
   * only search inside these, so the panel says which regions it covers and
   * offers to fetch the rest rather than quietly matching nothing.
   */
  zoneCache: Map<string, ZoneEntry[]>;
  /** Regions with at least one zone in the viewport, and where their content sits. */
  loadedRegions: RegionLoadSummary[];
  /** What those regions do to each other in world space; null below two regions. */
  mix: RegionMixReport | null;
  onUnloadRegion: (key: string) => void;
  /** Fetch (and cache) the zone lists of these regions so they become searchable. */
  onPrefetchRegions: (keys: string[]) => void;
  /** Display-only artificial layout for regions that would otherwise stack. */
  spread: boolean;
  spreadStepMetres: number;
  onToggleSpread: () => void;
  onSelectRegion: (key: string) => void;
  onToggleZone: (entry: ZoneEntry) => void;
  onLoadTile: (entries: ZoneEntry[]) => void;
  onUnloadTile: (entries: ZoneEntry[]) => void;
  /**
   * Load every zone of the SELECTED KINDS that the panel currently lists.
   * `scope` names those kinds ("Art", "Art + Gameplay", …) for the log and the
   * confirmation dialog — the shell never has to guess what the queue was.
   */
  onLoadAll: (entries: ZoneEntry[], scope: string) => void;
  onCancelBulk: () => void;
  bulk: BulkLoadProgress | null;
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
  zoneCache,
  loadedRegions,
  mix,
  onUnloadRegion,
  onPrefetchRegions,
  spread,
  spreadStepMetres,
  onToggleSpread,
  onSelectRegion,
  onToggleZone,
  onLoadTile,
  onUnloadTile,
  onLoadAll,
  onCancelBulk,
  bulk,
  onClearAll,
  onRefresh,
}: Props) {
  const [find, setFind] = useState("");
  const [filter, setFilter] = useState("");
  const [kinds, setKinds] = useState<Set<string>>(new Set());
  const [hideStubs, setHideStubs] = useState(false);
  const [collapsedGroups, setCollapsedGroups] = useState<Set<string>>(new Set());
  const [regionsOpen, setRegionsOpen] = useState(true);

  const needle = filter.trim().toLowerCase();

  /** The kind chips that are on, in the panel's own chip order. */
  const selectedKinds = useMemo(() => ZONE_KINDS.filter((k) => kinds.has(k)), [kinds]);

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

  /* ---------------- cross-group find ---------------- */

  const findQuery = find.trim();
  const findActive = findQuery.length >= 2;

  const foundRegions = useMemo(
    () => (findActive ? searchRegions(regions, findQuery) : []),
    [findActive, regions, findQuery],
  );

  const foundZones = useMemo(
    () => (findActive ? searchZones(zoneCache, findQuery, FIND_ZONE_LIMIT) : []),
    [findActive, zoneCache, findQuery],
  );

  /** Matched regions whose zone list has not been fetched, so nothing of theirs can match. */
  const unlistedMatches = useMemo(
    () => foundRegions.filter((r) => !zoneCache.has(r.key)).slice(0, PREFETCH_LIMIT),
    [foundRegions, zoneCache],
  );

  const foundGroups = useMemo(() => {
    const seen = new Set(foundRegions.map((r) => r.group));
    return GROUP_ORDER.filter((g) => seen.has(g));
  }, [foundRegions]);

  /** Same place, other name or other group — the trap this panel exists to defuse. */
  const siblings = useMemo(
    () => siblingRegions(regions, activeRegion),
    [regions, activeRegion],
  );

  const loadedByRegion = useMemo(
    () => new Map(loadedRegions.map((r) => [r.key, r])),
    [loadedRegions],
  );

  const shortRegionName = (key: string) => key.split("/").slice(2).join("/") || key;

  /**
   * Zones passing the search box and "Hide stubs" but NOT the kind chips —
   * the population the chip counts describe, so each chip can say how many
   * zones selecting it would put in scope.
   */
  const searchMatched = useMemo(
    () =>
      zones.filter((z) => {
        if (hideStubs && !z.hasObjects) return false;
        if (needle && !z.path.toLowerCase().includes(needle)) return false;
        return true;
      }),
    [zones, hideStubs, needle],
  );

  const kindCounts = useMemo(() => {
    const m = new Map<string, KindCount>();
    for (const k of ZONE_KINDS) m.set(k, { listed: 0, loadable: 0, pending: 0 });
    for (const z of searchMatched) {
      let c = m.get(z.kind);
      if (!c) {
        c = { listed: 0, loadable: 0, pending: 0 };
        m.set(z.kind, c);
      }
      c.listed++;
      if (z.hasObjects) {
        c.loadable++;
        if (!loadedPaths.has(z.path)) c.pending++;
      }
    }
    return m;
  }, [searchMatched, loadedPaths]);

  /** Loadable-but-unloaded zones across every kind, for the "not touched" line. */
  const pendingAllKinds = useMemo(() => {
    let n = 0;
    for (const c of kindCounts.values()) n += c.pending;
    return n;
  }, [kindCounts]);

  const loadableAllKinds = useMemo(() => {
    let n = 0;
    for (const c of kindCounts.values()) n += c.loadable;
    return n;
  }, [kindCounts]);

  const tiles = useMemo(() => {
    const groups = new Map<string, ZoneEntry[]>();
    for (const z of searchMatched) {
      if (kinds.size > 0 && !kinds.has(z.kind)) continue;
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
  }, [searchMatched, kinds, loadedPaths]);

  const shown = tiles.slice(0, TILE_RENDER_CAP);

  /**
   * What a "Load all" click queues: every listed zone OF THE SELECTED KINDS
   * that carries scene objects and is not loaded yet. Tiles past
   * {@link TILE_RENDER_CAP} are included — the cap only holds them out of the
   * DOM, not out of the filter.
   */
  const pendingLoad = useMemo(() => {
    if (selectedKinds.length === 0) return [];
    const out: ZoneEntry[] = [];
    for (const t of tiles) {
      for (const e of t.loadable) if (!loadedPaths.has(e.path)) out.push(e);
    }
    return out;
  }, [tiles, loadedPaths, selectedKinds]);

  /** "Art", "Art + Gameplay", "4 kinds" — short enough for a button face. */
  const scopeLabel = useMemo(() => {
    if (selectedKinds.length === 0) return null;
    if (selectedKinds.length === 1) return KIND_LABELS[selectedKinds[0]];
    if (selectedKinds.length === 2)
      return `${KIND_LABELS[selectedKinds[0]]} + ${KIND_LABELS[selectedKinds[1]]}`;
    return `${selectedKinds.length} kinds`;
  }, [selectedKinds]);

  /** Every selected kind spelled out, for tooltips and the confirm dialog. */
  const scopeNames = useMemo(
    () => selectedKinds.map((k) => KIND_LABELS[k]).join(", "),
    [selectedKinds],
  );

  const loadAllDisabled = selectedKinds.length === 0 || pendingLoad.length === 0;

  const loadAllTitle = useMemo(() => {
    const lines: string[] = [];
    if (selectedKinds.length === 0) {
      lines.push("This click would load 0 zones.");
      lines.push(
        "Pick a kind chip below — “Load all” only ever loads the kinds you select there.",
      );
      lines.push("Select several chips to load several kinds in one run.");
      if (pendingAllKinds > 0) {
        const kindsWithWork = [...kindCounts.values()].filter((c) => c.pending > 0).length;
        lines.push(
          `${pendingAllKinds.toLocaleString()} zone${
            pendingAllKinds === 1 ? " is" : "s are"
          } listed and not loaded, over ${kindsWithWork} kind${kindsWithWork === 1 ? "" : "s"}.`,
        );
      }
      return lines.join("\n");
    }

    const n = pendingLoad.length;
    const loadableInScope = selectedKinds.reduce(
      (acc, k) => acc + (kindCounts.get(k) ?? EMPTY_COUNT).loadable,
      0,
    );
    const alreadyInScope = loadableInScope - n;
    const outOfScope = pendingAllKinds - n;

    if (n > 0) {
      lines.push(
        selectedKinds.length === 1
          ? `Loads ${n.toLocaleString()} ${scopeNames} zone${n === 1 ? "" : "s"}.`
          : `Loads ${n.toLocaleString()} zone${n === 1 ? "" : "s"} of kinds ${scopeNames}.`,
      );
    } else if (loadableInScope === 0) {
      lines.push(
        `This click would load 0 zones — no listed ${scopeNames} zone carries scene objects.`,
      );
    } else {
      lines.push(
        `This click would load 0 zones — all ${loadableInScope.toLocaleString()} listed ${scopeNames} zones are already loaded.`,
      );
    }
    const conditions = [scopeNames];
    if (needle) conditions.push(`path contains “${filter.trim()}”`);
    if (hideStubs) conditions.push("non-stub only");
    lines.push(`Scope: ${conditions.join(", ")}.`);
    if (alreadyInScope > 0 && n > 0)
      lines.push(
        `${alreadyInScope.toLocaleString()} ${scopeNames} zone${
          alreadyInScope === 1 ? " is" : "s are"
        } already loaded and will be skipped.`,
      );
    if (outOfScope > 0)
      lines.push(
        `${outOfScope.toLocaleString()} loadable zone${
          outOfScope === 1 ? "" : "s"
        } of other kinds stay untouched.`,
      );
    if (tiles.length > TILE_RENDER_CAP)
      lines.push("Tiles held back by the render cap are included.");
    return lines.join("\n");
  }, [
    selectedKinds,
    pendingLoad,
    pendingAllKinds,
    kindCounts,
    scopeNames,
    needle,
    filter,
    hideStubs,
    tiles,
  ]);

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

  /**
   * Region rows count only the kinds the chips select, so the number next to a
   * region never promises more than "Load all" would take from it.
   */
  const regionCount = (r: LevelRegion) =>
    selectedKinds.length === 0
      ? r.zoneCount
      : selectedKinds.reduce((n, k) => n + (r.kinds[k] ?? 0), 0);

  const regionTitle = (r: LevelRegion) =>
    selectedKinds.length === 0
      ? `${r.key}\n${r.zoneCount.toLocaleString()} zones`
      : `${r.key}\n${regionCount(r).toLocaleString()} ${scopeNames} zones of ${r.zoneCount.toLocaleString()} total`;

  const regionRow = (r: LevelRegion, showGroup: boolean) => {
    const here = loadedByRegion.get(r.key);
    return (
      <button
        key={r.key}
        className={`${styles.regionRow} ${activeRegion === r.key ? styles.active : ""}`}
        onClick={() => onSelectRegion(r.key)}
        title={regionTitle(r)}
      >
        {showGroup && (
          <span className={styles.groupTag}>{GROUP_LABELS[r.group] ?? r.group}</span>
        )}
        <span className={styles.regionName}>{r.label}</span>
        {here && (
          <span className={styles.loadedTag} title={`${here.zones} zones of this region are loaded`}>
            {here.zones} loaded
          </span>
        )}
        <span className={styles.count}>{regionCount(r).toLocaleString()}</span>
      </button>
    );
  };

  return (
    <div className={styles.panel}>
      {/* One box over every group. The two Nefarious Cities differ by a single
          underscore and sit in different groups, so the only reliable way to
          reach the right one is to search names with the punctuation thrown
          away — see `squash` in RegionIndex.tsx. */}
      <div className={styles.findBox}>
        <LuSearch className={styles.findIcon} />
        <input
          className={styles.findInput}
          placeholder="Find a region or zone in any group…"
          value={find}
          onChange={(e) => setFind(e.target.value)}
        />
        {find && (
          <button className={styles.iconBtn} onClick={() => setFind("")} title="Clear">
            <LuX />
          </button>
        )}
      </div>

      {findActive && (
        <div className={styles.findResults}>
          <div className={styles.findHead}>
            {foundRegions.length.toLocaleString()} region
            {foundRegions.length === 1 ? "" : "s"}
            {foundGroups.length > 0 &&
              ` in ${foundGroups.map((g) => GROUP_LABELS[g] ?? g).join(", ")}`}
          </div>
          {foundRegions.length === 0 && (
            <div className={styles.hint}>No region name contains “{findQuery}”.</div>
          )}
          {foundRegions.slice(0, 40).map((r) => regionRow(r, true))}

          {unlistedMatches.length > 0 && (
            <button
              className={styles.findMore}
              onClick={() => onPrefetchRegions(unlistedMatches.map((r) => r.key))}
              title={
                "Zone names can only be searched inside regions whose zone list has been fetched.\n" +
                unlistedMatches.map((r) => r.key).join("\n")
              }
            >
              List the zones of {unlistedMatches.length} matching region
              {unlistedMatches.length === 1 ? "" : "s"} so their names are searchable too
            </button>
          )}

          {foundZones.length > 0 && (
            <>
              <div className={styles.findHead}>
                {foundZones.length.toLocaleString()}
                {foundZones.length >= FIND_ZONE_LIMIT ? "+" : ""} zone
                {foundZones.length === 1 ? "" : "s"} in the {zoneCache.size} listed region
                {zoneCache.size === 1 ? "" : "s"}
              </div>
              {foundZones.map(({ region, zone }) => (
                <button
                  key={zone.path}
                  className={`${styles.zoneRow} ${loadedPaths.has(zone.path) ? styles.zoneLoaded : ""} ${
                    zone.hasObjects ? "" : styles.zoneStub
                  }`}
                  onClick={() => onToggleZone(zone)}
                  title={`${zone.path}\n${shortRegionName(region)}\n${formatSize(zone.size)}`}
                >
                  <span className={styles.zoneCheck}>
                    {loadedPaths.has(zone.path) ? <LuCheck /> : null}
                  </span>
                  <span className={`${styles.kindTag} ${styles[`kind_${zone.kind}`] ?? ""}`}>
                    {zone.kind}
                  </span>
                  <span className={styles.zoneName}>{zoneFileName(zone.path)}</span>
                  <span className={styles.zoneMeta}>{shortRegionName(region)}</span>
                </button>
              ))}
            </>
          )}
        </div>
      )}

      <div className={styles.sectionHead}>
        <button
          className={styles.sectionToggle}
          onClick={() => setRegionsOpen((o) => !o)}
          disabled={findActive}
          title={findActive ? "Clear the find box to browse the region tree" : undefined}
        >
          {regionsOpen && !findActive ? <LuChevronDown /> : <LuChevronRight />}
          <span>Regions</span>
          <span className={styles.count}>{regions.length}</span>
        </button>
        <button className={styles.iconBtn} onClick={onRefresh} title="Reload the region index">
          <LuRefreshCw className={loadingRegions ? styles.spin : undefined} />
        </button>
      </div>

      {/* While the find box is active its results ARE the region list, so the
          tree steps aside — three stacked percentage-capped sections would
          otherwise leave the zone list no room. */}
      {regionsOpen && !findActive && (
        <div className={styles.regionList}>
          {loadingRegions && regions.length === 0 && (
            <div className={styles.hint}>Scanning archives…</div>
          )}
          {!loadingRegions && regions.length === 0 && (
            <div className={styles.hint}>No level zones found in this archive set.</div>
          )}
          {selectedKinds.length > 0 && regions.length > 0 && (
            <div className={styles.groupNote}>Counts show {scopeNames} zones only</div>
          )}
          {regionGroups.map((g) => (
            <div key={g.group}>
              <div className={styles.groupLabel}>{g.label}</div>
              {g.regions.map((r) => regionRow(r, false))}
            </div>
          ))}
        </div>
      )}

      {stats && (
        <div className={styles.statsBar} title="Totals for every level archive, not for the region below">
          <span className={styles.statsLabel}>Whole game</span>
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

      {loadedRegions.length > 0 && (
        <div className={styles.loadedRegions}>
          <div className={styles.loadedHead}>
            In the viewport
            <span className={styles.count}>
              {loadedRegions.length} region{loadedRegions.length === 1 ? "" : "s"}
            </span>
          </div>
          {loadedRegions.map((r) => (
            <div key={r.key} className={styles.loadedRow}>
              <span className={styles.groupTag}>{GROUP_LABELS[r.group] ?? r.group}</span>
              <button
                className={styles.loadedName}
                onClick={() => onSelectRegion(r.key)}
                title={`${r.key}\nShow this region's zone list`}
              >
                {shortRegionName(r.key)}
              </button>
              <span className={styles.loadedMeta}>
                {r.zones} zone{r.zones === 1 ? "" : "s"} · {r.objects.toLocaleString()} obj
              </span>
              <button
                className={styles.tileIconBtn}
                onClick={() => onUnloadRegion(r.key)}
                title={`Unload the ${r.zones} loaded zone${r.zones === 1 ? "" : "s"} of this region`}
              >
                <LuX />
              </button>
            </div>
          ))}

          {/* The level asset carries no per-region placement transform — regions
              hold whatever coordinates they were authored in. So the panel never
              claims the regions relate; it measures what they actually do and
              says that instead. */}
          {mix && mix.pairs.length > 0 && (
            <div className={styles.mixNotes}>
              {mix.pairs.slice(0, 4).map((pair) => (
                <div
                  key={`${pair.a}|${pair.b}`}
                  className={
                    pair.relation === "apart" ? styles.mixOk : styles.mixWarn
                  }
                >
                  {pair.relation !== "apart" && <LuTriangleAlert />}
                  <span>{relationSentence(pair, shortRegionName)}</span>
                </div>
              ))}
              {mix.truncated && (
                <div className={styles.mixOk}>
                  The duplicate scan stopped at its cap, so those counts are lower bounds.
                </div>
              )}
            </div>
          )}

          {loadedRegions.length > 1 && spreadStepMetres > 0 && (
            <button
              className={`${styles.spreadBtn} ${spread ? styles.spreadOn : ""}`}
              onClick={onToggleSpread}
              title={
                spread
                  ? `Each region after the first is drawn ${spreadStepMetres.toLocaleString()} m further along X than the one before. The offset is invented by this tool — the game has no such transform — so transform editing is off while it is on.`
                  : `Draw each region ${spreadStepMetres.toLocaleString()} m apart along X so they can be compared side by side. The offset is invented by this tool, not read from the level, and transform editing is off while it is on.`
              }
            >
              <LuMoveHorizontal />
              {spread
                ? `Side by side: +${spreadStepMetres.toLocaleString()} m per region (display only)`
                : "Lay regions out side by side"}
            </button>
          )}
        </div>
      )}

      <div className={styles.sectionHead}>
        <span className={styles.sectionTitle}>
          Zones{activeRegion ? ` · ${activeRegion.split("/").pop()}` : ""}
        </span>
        <div className={styles.headActions}>
          {bulk ? (
            <>
              <span className={styles.bulkProgress}>
                loading {bulk.done.toLocaleString()} / {bulk.total.toLocaleString()}
              </span>
              <button
                className={styles.linkBtn}
                onClick={onCancelBulk}
                title="Stop loading — the zones already loaded stay loaded"
              >
                Stop
              </button>
            </>
          ) : (
            <>
              {activeRegion && loadableAllKinds > 0 && (
                <button
                  className={styles.loadAllBtn}
                  onClick={() => onLoadAll(pendingLoad, scopeNames)}
                  disabled={loadAllDisabled}
                  title={loadAllTitle}
                >
                  <LuLayers3 />
                  {scopeLabel ? `Load all ${scopeLabel}` : "Load all"}
                  {pendingLoad.length > 0 && (
                    <span className={styles.count}>{pendingLoad.length.toLocaleString()}</span>
                  )}
                </button>
              )}
              {loadedPaths.size > 0 && (
                <button className={styles.linkBtn} onClick={onClearAll} title="Unload every zone">
                  Clear {loadedPaths.size}
                </button>
              )}
            </>
          )}
        </div>
      </div>

      <input
        className={styles.search}
        placeholder="Filter zones in this region by path…"
        value={filter}
        onChange={(e) => setFilter(e.target.value)}
      />

      {/* Nefarious City is authored twice: `openarea/nefarious_city` (1,176
          zones) and `instance/nefariouscity/nefariouscity` (28 zones, 91,245
          model instances), plus four more siblings. Nothing in the region list
          links them, so the panel links them here. */}
      {siblings.length > 0 && (
        <div className={styles.siblings}>
          <span className={styles.siblingLead}>
            Also authored as {siblings.length === 1 ? "another region" : `${siblings.length} other regions`}:
          </span>
          {siblings.slice(0, 6).map((r) => (
            <button
              key={r.key}
              className={styles.siblingBtn}
              onClick={() => onSelectRegion(r.key)}
              title={`${r.key}\n${r.zoneCount.toLocaleString()} zones · ${Object.entries(r.kinds)
                .map(([k, n]) => `${KIND_LABELS[k] ?? k} ${n}`)
                .join(", ")}`}
            >
              <span className={styles.groupTag}>{GROUP_LABELS[r.group] ?? r.group}</span>
              {shortRegionName(r.key)}
              <span className={styles.count}>{r.zoneCount.toLocaleString()}</span>
            </button>
          ))}
        </div>
      )}

      <div className={styles.chips}>
        {ZONE_KINDS.map((k) => {
          const c = kindCounts.get(k) ?? EMPTY_COUNT;
          const on = kinds.has(k);
          const title = [
            `${KIND_LABELS[k]} zones — click to ${on ? "drop from" : "add to"} the list and the “Load all” scope.`,
            `${c.listed.toLocaleString()} listed, ${c.loadable.toLocaleString()} with scene objects, ${c.pending.toLocaleString()} not loaded yet.`,
          ].join("\n");
          return (
            <button
              key={k}
              className={`${styles.chip} ${on ? styles.chipOn : ""}`}
              onClick={() => toggleKind(k)}
              disabled={c.listed === 0 && !on}
              title={title}
            >
              {KIND_LABELS[k]}
              <span className={styles.chipCount}>{c.listed.toLocaleString()}</span>
            </button>
          );
        })}
        <button
          className={`${styles.chip} ${hideStubs ? styles.chipOn : ""}`}
          onClick={() => setHideStubs((h) => !h)}
          title="Hide zones that contain no scene objects (they can never be loaded)"
        >
          Hide stubs
        </button>
      </div>

      {!bulk && activeRegion && loadableAllKinds > 0 && selectedKinds.length === 0 && (
        <div className={styles.scopeHint}>
          Pick a kind chip — “Load all” loads only the kinds selected there.
        </div>
      )}

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
                    title={
                      selectedKinds.length === 0
                        ? `Load the ${t.loadable.length} listed non-empty zones of this tile, every kind`
                        : `Load the ${t.loadable.length} listed non-empty ${scopeNames} zones of this tile`
                    }
                  >
                    <LuLayers />
                    Load tile
                  </button>
                )}
                {t.loadedCount > 0 && (
                  <button
                    className={styles.tileIconBtn}
                    onClick={() => onUnloadTile(t.entries)}
                    title={`Unload the ${t.loadedCount} loaded zone${t.loadedCount === 1 ? "" : "s"} of this tile`}
                  >
                    <LuX />
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
            rest. “Load all” still covers every one of them.
          </div>
        )}
      </div>
    </div>
  );
}
