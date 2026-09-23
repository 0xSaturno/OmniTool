/**
 * Finding a region, and knowing what its coordinates mean.
 *
 * Two problems live here, and they are the same problem seen from two ends.
 *
 * **Finding it.** Nefarious City exists twice under names that differ by one
 * underscore, in two different sidebar groups:
 *
 * ```text
 *   i29/openarea/nefarious_city                 1,176 zones, 105,432 model instances
 *   i29/instance/nefariouscity/nefariouscity       28 zones,  91,245 model instances
 * ```
 *
 * plus `nefariouscity_tower/*`, `nefariouscity_actone`, `nefariouscity_vs2` and
 * a planet-transition region — fifteen regions across three groups in all. A
 * user who picks the wrong one sees a different city and has no way to tell.
 * {@link squash} throws away everything that is not a letter or a digit, so one
 * query finds every spelling, and {@link siblingRegions} names the alternatives
 * on the region the user did land on.
 *
 * **Knowing what it means once loaded.** The level format has NO per-region
 * placement transform — see the module comment on `src-tauri/src/core/level_asset.rs`
 * and the `instance_placement` harness for the proof. A region's zones carry
 * whatever world coordinates they were authored in, and nothing in the level
 * relates one region's space to another's. Measured on the shipped level, that
 * lands in three different places:
 *
 *   * `openarea/nefarious_city` and `instance/nefariouscity/nefariouscity`
 *     share a space so completely that 75,107 of the open area's 98,866
 *     distinct (model, world metre) instances also exist in the instance
 *     region — 76%. Loading both draws the same city twice.
 *   * `instance/nefariouscity_actone` shares exactly 0 with the open area: a
 *     separate space that happens to sit in the same numeric neighbourhood.
 *   * 38 of 96 instance regions are centred within 500 m of the world origin,
 *     so several unrelated ones stack on top of each other if loaded together.
 *
 * None of that is knowable from the region list, so {@link analyseRegionMix}
 * measures it from the zones actually in the viewport and the browser states
 * the finding instead of implying a relationship the data does not carry.
 */

import type { LevelRegion, ZoneEntry } from "../api";

/* ------------------------------------------------------------------ */
/* Names                                                               */
/* ------------------------------------------------------------------ */

/**
 * Lowercase, letters and digits only.
 *
 * `nefarious_city`, `Nefarious City` and `nefariouscity` all collapse to
 * `nefariouscity`, which is the entire reason the two Nefarious Cities were
 * hard to find.
 */
export function squash(s: string): string {
  let out = "";
  for (const ch of s.toLowerCase()) {
    if ((ch >= "a" && ch <= "z") || (ch >= "0" && ch <= "9")) out += ch;
  }
  return out;
}

/**
 * The region key a zone asset path belongs to.
 *
 * Mirrors `classify()` in `src-tauri/src/core/level.rs` exactly, including its
 * fallbacks, because the shell has to map a loaded zone back to its region
 * without another IPC round trip. {@link resolveRegionKey} checks the answer
 * against the real region list, so a drift in either direction degrades to
 * "unknown region" rather than to a wrong label.
 *
 * `levels/<level>/<group>/<region…>/[tile_<xy>/]<file>.zone`
 */
export function regionKeyOfZonePath(path: string): string {
  const parts = path.split("/");
  if (parts.length < 3 || parts[0] !== "levels") {
    const dir = parts.slice(0, Math.max(0, parts.length - 1)).join("/");
    return dir === "" ? "misc" : dir;
  }
  const level = parts[1];
  const rest = parts.slice(2, parts.length - 1);
  if (rest.length === 0) return `${level}/(root)`;

  const groupDir = rest[0];
  let regionDirs = rest.slice(1);
  if (regionDirs.length > 0 && regionDirs[regionDirs.length - 1].startsWith("tile_")) {
    regionDirs = regionDirs.slice(0, regionDirs.length - 1);
  }
  const sub = regionDirs.length === 0 ? groupDir : regionDirs.join("/");
  return `${level}/${groupDir}/${sub}`;
}

/**
 * The derived key, but only when the region index agrees it exists. Falls back
 * to the longest known key the path starts with, then to `null`.
 */
export function resolveRegionKey(path: string, known: Set<string>): string | null {
  const derived = regionKeyOfZonePath(path);
  if (known.has(derived)) return derived;
  let best: string | null = null;
  for (const key of known) {
    if (path.startsWith(`${key}/`) && (best === null || key.length > best.length)) best = key;
  }
  return best;
}

/**
 * The family name a region belongs to: the first path component below the
 * group, squashed. `i29/openarea/nefarious_city` and
 * `i29/instance/nefariouscity/nefariouscity` both give `nefariouscity`.
 */
export function regionFamily(key: string): string {
  const parts = key.split("/");
  return squash(parts.length >= 3 ? parts[2] : parts[parts.length - 1] ?? "");
}

/** A family name short enough to collide by accident is not used for siblings. */
const MIN_FAMILY_LEN = 6;

/**
 * Regions that are plausibly the same place as `key` under another name or in
 * another group, most content first.
 *
 * Two regions are siblings when one family name is a prefix of the other and
 * the shorter is at least {@link MIN_FAMILY_LEN} characters — enough to tie
 * `nefariouscity` to `nefariouscity_tower` without tying every `mis_*` overlay
 * to every other.
 */
export function siblingRegions(regions: LevelRegion[], key: string | null): LevelRegion[] {
  if (!key) return [];
  const mine = regionFamily(key);
  if (mine.length < MIN_FAMILY_LEN) return [];
  return regions
    .filter((r) => {
      if (r.key === key) return false;
      const theirs = regionFamily(r.key);
      if (theirs.length < MIN_FAMILY_LEN) return false;
      return theirs.startsWith(mine) || mine.startsWith(theirs);
    })
    .sort((a, b) => b.zoneCount - a.zoneCount);
}

/** Regions whose key or label contains the squashed query, best match first. */
export function searchRegions(regions: LevelRegion[], query: string): LevelRegion[] {
  const needle = squash(query);
  if (needle.length < 2) return [];
  const scored: { r: LevelRegion; score: number }[] = [];
  for (const r of regions) {
    const key = squash(r.key);
    const family = regionFamily(r.key);
    if (!key.includes(needle)) continue;
    // An exact family hit ("nefariouscity") outranks a region that merely
    // contains the text somewhere in a longer path.
    const score = family === needle ? 0 : family.startsWith(needle) ? 1 : 2;
    scored.push({ r, score });
  }
  scored.sort((a, b) => a.score - b.score || b.r.zoneCount - a.r.zoneCount);
  return scored.map((s) => s.r);
}

/** Zones of any already-listed region whose path contains the query. */
export function searchZones(
  cache: Map<string, ZoneEntry[]>,
  query: string,
  limit: number,
): { region: string; zone: ZoneEntry }[] {
  const needle = query.trim().toLowerCase();
  if (needle.length < 2) return [];
  const out: { region: string; zone: ZoneEntry }[] = [];
  for (const [region, zones] of cache) {
    for (const z of zones) {
      if (!z.path.toLowerCase().includes(needle)) continue;
      out.push({ region, zone: z });
      if (out.length >= limit) return out;
    }
  }
  return out;
}

/* ------------------------------------------------------------------ */
/* Space                                                               */
/* ------------------------------------------------------------------ */

export interface RegionLoadSummary {
  key: string;
  label: string;
  group: string;
  zones: number;
  objects: number;
  /** World AABB over this region's loaded zones, or null when none reported one. */
  min: [number, number, number] | null;
  max: [number, number, number] | null;
}

export type RegionRelation = "coincident" | "intersecting" | "apart";

export interface RegionMixReport {
  regions: RegionLoadSummary[];
  /** Pairwise verdicts, worst first. Empty while fewer than two regions load. */
  pairs: {
    a: string;
    b: string;
    relation: RegionRelation;
    /** Model instances of `b` sitting on an identical instance of `a`. */
    duplicates: number;
  }[];
  /** True when the duplicate scan was cut short; counts are then lower bounds. */
  truncated: boolean;
}

/** Above this many model instances the duplicate scan stops and says so. */
const DUPLICATE_SCAN_CAP = 400_000;

/** A pair is "coincident" once this share of the smaller region is duplicated. */
const COINCIDENT_SHARE = 0.2;

function overlapsXZ(a: RegionLoadSummary, b: RegionLoadSummary): boolean {
  if (!a.min || !a.max || !b.min || !b.max) return false;
  return a.min[0] <= b.max[0] && b.min[0] <= a.max[0] && a.min[2] <= b.max[2] && b.min[2] <= a.max[2];
}

/**
 * What the regions currently in the viewport do to each other.
 *
 * The duplicate count is the honest half: the same `.model` asset at the same
 * world metre in two regions is not a coincidence, it is the same object
 * authored twice, and drawing both is z-fighting rather than more of the map.
 * Bounds overlap alone is weaker — two regions really can share a world — so it
 * is reported separately and worded as "intersect", not as "wrong".
 */
export function analyseRegionMix(
  summaries: RegionLoadSummary[],
  instances: { region: string; modelId: string; x: number; y: number; z: number }[],
): RegionMixReport {
  const report: RegionMixReport = { regions: summaries, pairs: [], truncated: false };
  if (summaries.length < 2) return report;

  const scan = instances.length > DUPLICATE_SCAN_CAP ? instances.slice(0, DUPLICATE_SCAN_CAP) : instances;
  report.truncated = scan.length < instances.length;

  // key -> the regions that place an identical instance there
  const seen = new Map<string, Set<string>>();
  for (const i of scan) {
    if (!i.modelId || i.modelId === "0x0") continue;
    const key = `${i.modelId}|${Math.round(i.x)}|${Math.round(i.y)}|${Math.round(i.z)}`;
    let set = seen.get(key);
    if (!set) {
      set = new Set();
      seen.set(key, set);
    }
    set.add(i.region);
  }

  const dupes = new Map<string, number>();
  for (const set of seen.values()) {
    if (set.size < 2) continue;
    const keys = [...set].sort();
    for (let i = 0; i < keys.length; i++) {
      for (let j = i + 1; j < keys.length; j++) {
        const pair = `${keys[i]}\u0000${keys[j]}`;
        dupes.set(pair, (dupes.get(pair) ?? 0) + 1);
      }
    }
  }

  const byKey = new Map(summaries.map((s) => [s.key, s]));
  for (let i = 0; i < summaries.length; i++) {
    for (let j = i + 1; j < summaries.length; j++) {
      const a = summaries[i];
      const b = summaries[j];
      const pair = [a.key, b.key].sort().join("\u0000");
      const duplicates = dupes.get(pair) ?? 0;
      const smaller = Math.min(
        byKey.get(a.key)?.objects ?? 0,
        byKey.get(b.key)?.objects ?? 0,
      );
      const relation: RegionRelation =
        smaller > 0 && duplicates / smaller >= COINCIDENT_SHARE
          ? "coincident"
          : overlapsXZ(a, b)
            ? "intersecting"
            : "apart";
      report.pairs.push({ a: a.key, b: b.key, relation, duplicates });
    }
  }

  const rank: Record<RegionRelation, number> = { coincident: 0, intersecting: 1, apart: 2 };
  report.pairs.sort((p, q) => rank[p.relation] - rank[q.relation] || q.duplicates - p.duplicates);
  return report;
}

/**
 * The artificial X step "Spread regions apart" uses.
 *
 * It is the widest loaded region rounded up to a round number of kilometres,
 * so the number in the UI is obviously a display device and not something the
 * game would ever produce. Zero when a single region is loaded.
 */
export function spreadStep(summaries: RegionLoadSummary[]): number {
  let widest = 0;
  for (const s of summaries) {
    if (!s.min || !s.max) continue;
    widest = Math.max(widest, s.max[0] - s.min[0], s.max[2] - s.min[2]);
  }
  if (widest <= 0) return 0;
  return Math.max(1000, Math.ceil((widest * 1.25) / 1000) * 1000);
}

/* ------------------------------------------------------------------ */
/* Presentation                                                        */
/* ------------------------------------------------------------------ */

/** Human sentence for one pair verdict; the browser prints these verbatim. */
export function relationSentence(
  pair: RegionMixReport["pairs"][number],
  shortName: (key: string) => string,
): string {
  const a = shortName(pair.a);
  const b = shortName(pair.b);
  switch (pair.relation) {
    case "coincident":
      return `${a} and ${b} are the same place authored twice — ${pair.duplicates.toLocaleString()} instances sit on identical copies of each other. Loading both draws the city twice.`;
    case "intersecting":
      return `${a} and ${b} occupy overlapping coordinates. The level carries no per-region placement, so this is what the two were authored at — it may be one space, or two spaces that happen to sit on the same numbers.`;
    default:
      return `${a} and ${b} sit apart in world coordinates.`;
  }
}
