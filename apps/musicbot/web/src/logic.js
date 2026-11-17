// SPDX-License-Identifier: EUPL-1.2
// Pure helpers without DOM or Svelte dependencies, tested in logic.test.js.

/** Formats seconds as `3:07` or `1:02:03`. Invalid input gives an empty string. */
export function formatDuration(seconds) {
  if (typeof seconds !== "number" || !Number.isFinite(seconds)) return "";
  const total = Math.max(0, Math.floor(seconds));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = String(total % 60).padStart(2, "0");
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${s}` : `${m}:${s}`;
}

/** Sum of the known durations of tracks, in seconds. */
export function totalDuration(tracks) {
  let sum = 0;
  for (const track of tracks) if (typeof track?.duration === "number") sum += track.duration;
  return sum;
}

/**
 * Position in ms shown for `current`: `position_ms` plus the time since the
 * snapshot arrived while `advancing`, never below 0 and capped at the duration
 * (seconds, may be null).
 */
export function interpolatePosition(positionMs, advancing, receivedAt, now, durationSeconds) {
  let ms = positionMs + (advancing ? Math.max(0, now - receivedAt) : 0);
  if (typeof durationSeconds === "number" && durationSeconds > 0) ms = Math.min(ms, durationSeconds * 1000);
  return Math.max(0, ms);
}

export const clamp = (value, min, max) => Math.min(max, Math.max(min, value));

/** Fraction 0..1 of a pointer at `clientX` over a box starting at `left` with `width`. */
export function pointerFraction(clientX, left, width) {
  if (!(width > 0)) return 0;
  return clamp((clientX - left) / width, 0, 1);
}

/**
 * New slider value in ms for a key on the progress slider, or null when the
 * key does nothing. Arrows move 5 s, Page keys 30 s, Home/End jump to the ends.
 */
export function seekKeyTarget(key, valueMs, maxMs) {
  const steps = { ArrowLeft: -5000, ArrowDown: -5000, ArrowRight: 5000, ArrowUp: 5000, PageDown: -30000, PageUp: 30000 };
  if (key === "Home") return 0;
  if (key === "End") return maxMs;
  if (key in steps) return clamp(valueMs + steps[key], 0, maxMs);
  return null;
}

/**
 * Gap index (0..length) for a pointer over the row at `rowIndex`: the upper
 * half of a row means before it, the lower half after it.
 */
export function gapIndex(rowIndex, offsetY, rowHeight) {
  return rowIndex + (offsetY >= rowHeight / 2 ? 1 : 0);
}

/**
 * True when the server has caught up with a seek to `targetMs` on entry
 * `entryId`: the song changed or the reported position is within 1.5 s of the target.
 */
export function seekSettled(targetMs, entryId, current, serverMs) {
  if (!current || current.entry.id !== entryId) return true;
  return Math.abs(serverMs - targetMs) < 1500;
}

/**
 * Index for the `move` command when the entry at `fromIndex` is dropped into
 * gap `gap` (0..length, counted in the list WITH the entry). The command wants
 * the index in the list WITHOUT the entry, so gaps below the entry shift up by
 * one. Returns null when the drop leaves the order unchanged.
 */
export function moveTarget(fromIndex, gap, length) {
  if (fromIndex < 0 || fromIndex >= length) return null;
  const g = clamp(gap, 0, length);
  const target = g > fromIndex ? g - 1 : g;
  return target === fromIndex ? null : target;
}

/** Copy of `list` with the item at `fromIndex` moved to `target` (index in the list without it, clamped). */
export function applyMove(list, fromIndex, target) {
  const rest = list.slice();
  const [item] = rest.splice(fromIndex, 1);
  if (item === undefined) return list.slice();
  rest.splice(clamp(target, 0, rest.length), 0, item);
  return rest;
}

/** Target index for moving the entry at `index` one step up (-1) or down (+1), or null at the ends. */
export function stepTarget(index, delta, length) {
  const target = index + delta;
  return target < 0 || target >= length ? null : target;
}

export const ALBUM_LISTS = [
  ["newest", "Recently added"],
  ["recent", "Recently played"],
  ["frequent", "Most played"],
  ["random", "Random"],
  ["alphabeticalByName", "By name"],
  ["starred", "Starred"],
];
const LIST_NAMES = new Set(ALBUM_LISTS.map(([name]) => name));

function libraryIndex(text) {
  const n = Number(text);
  return Number.isInteger(n) && n >= 0 ? n : 0;
}

/**
 * Parses a location hash into a route object:
 * `{ view: "queue" | "channels" | "history" }`, `{ view: "albums", library, list }`,
 * `{ view: "artists" | "playlists", library }`,
 * `{ view: "album" | "artist" | "playlist" | "song" | "similar", library, id, namespace? }`,
 * `{ view: "path", library, from, to, namespace? }`, `{ view: "search", q }`,
 * `{ view: "add", q }`, `{ view: "addAlbum", id }`, `{ view: "requests" }`.
 * A shared link (`library_key=`, see shareRoute) adds `sharedLibrary: key` to
 * library routes instead of a namespace; adoptSharedRoute resolves it.
 * Unknown hashes give the queue view; an invalid import album id gives the add view.
 */
export function parseRoute(hash) {
  const text = (hash || "").replace(/^#/, "");
  const question = text.indexOf("?");
  const path = question < 0 ? text : text.slice(0, question);
  const params = new URLSearchParams(question < 0 ? "" : text.slice(question + 1));
  const parts = path.split("/").filter((part) => part !== "");
  const [view, library, id, to] = parts;
  let decoded;
  let decodedTo;
  try {
    decoded = id === undefined ? undefined : decodeURIComponent(id);
    decodedTo = to === undefined ? undefined : decodeURIComponent(to);
  } catch {
    return { view: "queue" };
  }
  const shared = params.has("library_key") ? { sharedLibrary: params.get("library_key") } : null;
  const namespace = shared ?? (params.has("namespace") ? { namespace: params.get("namespace") } : {});
  switch (view) {
    case "channels":
    case "history":
    case "requests":
      return { view };
    case "albums": {
      const list = params.get("list");
      return { view, library: libraryIndex(params.get("library")), list: LIST_NAMES.has(list) ? list : "newest", ...shared };
    }
    case "artists":
    case "playlists":
      return { view, library: libraryIndex(params.get("library")), ...shared };
    case "album":
    case "artist":
    case "playlist":
    case "song":
    case "similar":
      if (decoded === undefined || parts.length !== 3) return { view: "queue" };
      return { view, library: libraryIndex(library), id: decoded, ...namespace };
    case "path":
      if (decodedTo === undefined || parts.length !== 4) return { view: "queue" };
      return { view, library: libraryIndex(library), from: decoded, to: decodedTo, ...namespace };
    case "search":
      return { view, q: params.get("q") ?? "" };
    case "add":
      if (parts.length === 1) return { view, q: params.get("q") ?? "" };
      if (parts.length === 3 && library === "album" && isImportId(decoded)) return { view: "addAlbum", id: decoded };
      return { view, q: "" };
    default:
      return { view: "queue" };
  }
}

/** Builds the hash (with leading `#`) for a route object; the inverse of parseRoute. */
export function buildRoute(route) {
  switch (route.view) {
    case "history":
      return "#/history";
    case "requests":
      return "#/requests";
    case "channels":
      return "#/channels";
    case "albums":
      return `#/albums?${new URLSearchParams({ library: String(route.library ?? 0), list: route.list ?? "newest" })}`;
    case "artists":
    case "playlists":
      return `#/${route.view}?library=${route.library ?? 0}`;
    case "album":
    case "artist":
    case "playlist":
    case "song":
    case "similar":
      return `#/${route.view}/${route.library ?? 0}/${encodeURIComponent(route.id)}${namespaceQuery(route)}`;
    case "path":
      return `#/path/${route.library ?? 0}/${encodeURIComponent(route.from)}/${encodeURIComponent(route.to)}${namespaceQuery(route)}`;
    case "search":
      return `#/search?${new URLSearchParams({ q: route.q ?? "" })}`;
    case "add":
      return `#/add?${new URLSearchParams({ q: route.q ?? "" })}`;
    case "addAlbum":
      return `#/add/album/${encodeURIComponent(route.id)}`;
    default:
      return "#/queue";
  }
}

function namespaceQuery(route) {
  return route.namespace == null ? "" : `?${new URLSearchParams({ namespace: route.namespace })}`;
}

/** The browse view to show for `route` in another library. */
export function routeForLibrary(route, library) {
  switch (route.view) {
    case "albums":
      return { ...route, library };
    case "artists":
    case "artist":
      return { view: "artists", library };
    case "playlists":
    case "playlist":
      return { view: "playlists", library };
    case "album":
    case "song":
      return { view: "albums", library, list: "newest" };
    case "similar":
    case "path":
      return { view: "queue" };
    default:
      return route;
  }
}

/** Keep a selected library by key (`library_keys`) only when that key is unambiguous in both namespaces. */
export function libraryForNamespace(previous, current, index) {
  const name = previous[index];
  if (name === undefined || previous.indexOf(name) !== previous.lastIndexOf(name)) return 0;
  const next = current.indexOf(name);
  return next >= 0 && next === current.lastIndexOf(name) ? next : 0;
}

const DETAIL_VIEWS = new Set(["album", "artist", "playlist", "song", "similar", "path"]);

/** Detail IDs in an unproved or expired namespace must return to their safe list. */
export function routeForNamespace(route, library, namespace) {
  if (DETAIL_VIEWS.has(route.view) && (route.library !== library || !namespace || route.namespace !== namespace)) {
    return routeForLibrary(route, library);
  }
  return typeof route.library === "number" && route.library !== library ? { ...route, library } : route;
}

/** The index of `key` in `keys`, or -1 when it is missing or ambiguous. */
function uniqueLibraryIndex(keys, key) {
  const index = keys.indexOf(key);
  return index >= 0 && index === keys.lastIndexOf(key) ? index : -1;
}

/**
 * The hash for sharing `route` with other viewers: no namespace, and a route
 * with a library carries its key (`library_key=`, from the state's
 * `library_keys`) because each viewer's indexes may differ. The key changes
 * with the library's name or URL, so an old link never sends its item ID to a
 * different backend. Null when the library index is invalid or its key is
 * shared with another library.
 */
export function shareRoute(route, libraryKeys) {
  const { namespace: _namespace, sharedLibrary: _shared, ...rest } = route;
  const hash = buildRoute(rest);
  if (typeof rest.library !== "number") return hash;
  const key = libraryKeys[rest.library];
  if (key === undefined || uniqueLibraryIndex(libraryKeys, key) !== rest.library) return null;
  return `${hash}${hash.includes("?") ? "&" : "?"}${new URLSearchParams({ library_key: key })}`;
}

/** The address bar hash for `route`: the share format, or (ambiguous library key) the route without namespace. */
export function addressRoute(route, libraryKeys) {
  const { namespace: _namespace, sharedLibrary: _shared, ...rest } = route;
  return shareRoute(rest, libraryKeys) ?? buildRoute(rest);
}

/**
 * The page's public root directory, without query or fragment. Defence in
 * depth: a legacy `/u/<token>/...` page path is cut at its first `/u/`.
 */
export function shareRoot(href) {
  const url = new URL(href);
  const legacy = /^(.*?\/)u\/[^/]+(?:\/.*)?$/.exec(url.pathname);
  return `${url.origin}${legacy ? legacy[1] : url.pathname.replace(/[^/]*$/, "")}`;
}

/** The page URL for `route` that works for every viewer with their own link, or null when it cannot be shared. */
export function shareUrl(href, route, libraryKeys) {
  const hash = shareRoute(route, libraryKeys);
  return hash === null ? null : `${shareRoot(href)}${hash}`;
}

/**
 * Resolves a shared route's library key in this viewer's libraries once
 * `namespace` exists. A missing, unknown or duplicated key falls back to the
 * first library's safe list.
 */
export function adoptSharedRoute(route, libraryKeys, namespace) {
  if (route.sharedLibrary == null || !namespace) return route;
  const { sharedLibrary, ...rest } = route;
  const library = uniqueLibraryIndex(libraryKeys, sharedLibrary);
  if (library < 0) return routeForLibrary(rest, 0);
  return DETAIL_VIEWS.has(rest.view) ? { ...rest, library, namespace } : { ...rest, library };
}

/** The shared Add/drag boundary: a captured namespace and a currently valid library index. */
export function browseItemIsCurrent(item, namespace, libraryCount) {
  return typeof namespace === "string" && namespace !== "" && item?.namespace === namespace
    && Number.isInteger(item.library) && item.library >= 0 && item.library < libraryCount;
}

/** An OpenSubsonic similarity (0..1) as an integer percent, or null when the server gave none. */
export function similarityPercent(value) {
  return typeof value === "number" && Number.isFinite(value) && value >= 0 && value <= 1 ? Math.round(value * 100) : null;
}

/** Failed loads of one cover after which it stays a placeholder. */
export const COVER_RETRIES = 6;

/**
 * Milliseconds to wait before loading a cover again after `failures` failed
 * loads: doubling from 250 up to 5 s (the bot refuses covers while its
 * artwork worker is busy), or null once it has failed `COVER_RETRIES` times.
 */
export function coverRetryDelay(failures) {
  if (!Number.isInteger(failures) || failures < 1 || failures > COVER_RETRIES) return null;
  return Math.min(5000, 250 * 2 ** (failures - 1));
}

/** The now-playing track id a sonic path to `song` can start from, or null when there is no such path. */
export function sonicPathStart(playing, song) {
  return playing && song && playing.library === song.library && typeof playing.id === "string" && playing.id !== "" && playing.id !== song.id ? playing.id : null;
}

/**
 * Calls `fn` once `ms` after the last call, with the last arguments.
 * `flush()` runs a pending call now, `cancel()` drops it.
 */
export function debounce(fn, ms) {
  let timer = null;
  let pending = null;
  const debounced = (...args) => {
    pending = args;
    clearTimeout(timer);
    timer = setTimeout(debounced.flush, ms);
  };
  debounced.flush = () => {
    clearTimeout(timer);
    timer = null;
    if (pending === null) return;
    const args = pending;
    pending = null;
    fn(...args);
  };
  debounced.cancel = () => {
    clearTimeout(timer);
    timer = null;
    pending = null;
  };
  debounced.pending = () => pending !== null;
  return debounced;
}

/**
 * localStorage key for the selected queue, per page path (normally the bot's
 * public root). The path carries no token, so the choice belongs to this
 * browser and root and outlives a change of remembered link.
 */
export function queueStorageKey(pathname) {
  return `nuschl-musicbot:queue:${pathname}`;
}

/** Index of the queue to select from a stored value, clamped to the available queues. */
export function selectedQueue(stored, count) {
  const n = Number(stored);
  if (!Number.isInteger(n) || n < 0 || count <= 0) return 0;
  return Math.min(n, count - 1);
}

export const DRAG_TYPE = "application/x-nuschl-musicbot";
export const DRAG_ENTRY_TYPE = "application/x-nuschl-musicbot-entry";
const KINDS = new Set(["song", "album", "playlist"]);

/** JSON for a library item dragged into the queue, carrying its originating namespace. */
export function encodeDragItem(kind, library, id, namespace) {
  return JSON.stringify({ kind, library, id, namespace });
}

/** Parses dragged JSON back into an item with its originating namespace, or null for invalid data. */
export function decodeDragItem(text) {
  let value;
  try {
    value = JSON.parse(text);
  } catch {
    return null;
  }
  if (!value || !KINDS.has(value.kind) || !Number.isInteger(value.library) || value.library < 0) return null;
  if (typeof value.id !== "string" || value.id === "" || typeof value.namespace !== "string" || value.namespace === "") return null;
  return { kind: value.kind, library: value.library, id: value.id, namespace: value.namespace };
}

/** True when a keyboard event target is a control that handles Space itself. */
export function handlesSpace(target) {
  if (!target || typeof target.closest !== "function") return false;
  return target.closest("input, textarea, select, button, a[href], [contenteditable=''], [contenteditable='true'], [role='slider']") !== null;
}

/** Merge an authoritative history page without discarding already loaded older plays. */
export function mergeHistory(current, page, older = false) {
  const retained = current.items.filter((item) => page.oldest_id !== null && item.id >= page.oldest_id);
  const items = [...new Map([...retained, ...page.items].map((item) => [item.id, item])).values()]
    .sort((a, b) => b.id - a.id).slice(0, 1000);
  const last = page.items.at(-1)?.id;
  const joins = last !== undefined && retained.some((item) => item.id >= last - 1);
  let next_before = older || !joins ? page.next_before : current.next_before;
  if (items.length === 1000 || (next_before !== null && next_before <= page.oldest_id)) next_before = null;
  return { items, next_before, oldest_id: page.oldest_id, revision: page.revision };
}

/** True for an import service id: exactly 16 lowercase hex characters. */
export function isImportId(id) {
  return typeof id === "string" && /^[0-9a-f]{16}$/.test(id);
}

const RELEASE_TYPES = { album: "Album", ep: "EP", single: "Single", compilation: "Compilation" };

/** Display name of an import service release type, or an empty string for an unknown one. */
export function releaseTypeLabel(type) {
  return Object.hasOwn(RELEASE_TYPES, type) ? RELEASE_TYPES[type] : "";
}

const count = (value) => (Number.isInteger(value) && value >= 0 ? value : 0);

/** The status line of an import request. */
export function requestStatusText(request) {
  const total = count(request?.tracks_total);
  switch (request?.status) {
    case "queued":
      return "Waiting to start";
    case "downloading":
      return `Adding · ${count(request.tracks_done)} of ${total} tracks`;
    case "handed_over": {
      const failed = Math.min(count(request.tracks_failed), total);
      return failed === 0
        ? "Added. Shows up in the library within a few minutes."
        : `Added ${total - failed} of ${total} tracks. They show up in the library within a few minutes.`;
    }
    case "failed":
      return "Could not be added.";
    default:
      return "";
  }
}

/** Progress of a request that is being added as `{ value, max }`, or null for any other state. */
export function requestProgress(request) {
  if (request?.status !== "downloading") return null;
  const max = count(request.tracks_total);
  return max > 0 ? { value: Math.min(count(request.tracks_done), max), max } : null;
}

/** Tracks still available in an import budget `{ limit, used }`, or null when it is malformed. */
function remaining(budget) {
  const { limit, used } = budget ?? {};
  return Number.isInteger(limit) && Number.isInteger(used) && limit >= 0 && used >= 0 ? Math.max(0, limit - used) : null;
}

/** The budget line for `{ mine, total }`: the smaller remainder of both, or null when either is malformed. */
export function budgetText(budget) {
  const mine = remaining(budget?.mine);
  const total = remaining(budget?.total);
  if (mine === null || total === null) return null;
  const left = Math.min(mine, total);
  return `You can add ${left} more ${left === 1 ? "track" : "tracks"} today.`;
}

const byName = (a, b) => String(a.name).localeCompare(String(b.name));
// Favorites first, then the caller's own, then by name.
const playlistOrder = (a, b) => Number(b.favorites === true) - Number(a.favorites === true) || Number(b.mine === true) - Number(a.mine === true) || byName(a, b);

/**
 * The Playlists page's sections: the caller's own (Favorites first), other
 * people's personal playlists by name, then the library's in its order.
 */
export function groupPlaylists(playlists) {
  const list = Array.isArray(playlists) ? playlists : [];
  return {
    mine: list.filter((playlist) => playlist.personal && playlist.mine).sort(playlistOrder),
    others: list.filter((playlist) => playlist.personal && !playlist.mine).sort(byName),
    library: list.filter((playlist) => !playlist.personal),
  };
}

/** The playlists the caller may add songs to, for the "Add to playlist" menu. */
export function editablePlaylists(playlists) {
  return (Array.isArray(playlists) ? playlists : []).filter((playlist) => playlist.editable === true).sort(playlistOrder);
}

/**
 * Index of the synced lyric line sung at `ms`: the last line that has begun,
 * or -1 before the first. `lines` are ordered by `start_ms`.
 */
export function lyricLineAt(lines, ms) {
  if (!Array.isArray(lines) || !Number.isFinite(ms)) return -1;
  let low = 0;
  let high = lines.length;
  while (low < high) {
    const middle = (low + high) >> 1;
    if ((lines[middle]?.start_ms ?? Infinity) <= ms) low = middle + 1;
    else high = middle;
  }
  return low - 1;
}

/** The Artists page's jump targets: name and index of every group that has artists. */
export function artistJumps(groups) {
  return (Array.isArray(groups) ? groups : []).flatMap((group, index) =>
    Array.isArray(group?.artists) && group.artists.length ? [{ name: String(group.name), index }] : []);
}

/** Shortest text, in characters, the search field asks suggestions for. */
export const SUGGEST_MIN_CHARS = 2;

/** The text to ask suggestions for: `text` trimmed, or null when it is shorter than `SUGGEST_MIN_CHARS`. */
export function suggestionQuery(text) {
  const trimmed = typeof text === "string" ? text.trim() : "";
  return [...trimmed].length >= SUGGEST_MIN_CHARS ? trimmed : null;
}

/**
 * The highlighted suggestion after one step `delta` (+1 down, -1 up) from
 * `active` in a list of `length`: wraps around, and -1 (nothing) steps to the
 * first or last item.
 */
export function suggestionStep(active, delta, length) {
  if (!(length > 0)) return -1;
  if (active < 0 || active >= length) return delta > 0 ? 0 : length - 1;
  return (active + delta + length) % length;
}
