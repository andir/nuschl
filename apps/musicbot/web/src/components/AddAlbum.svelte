<!-- SPDX-License-Identifier: EUPL-1.2 -->
<script>
  import { store } from "../store.svelte.js";
  import { resource } from "../resource.svelte.js";
  import { postForJSON, query } from "../api.js";
  import { buildRoute, formatDuration, releaseTypeLabel, requestProgress, requestStatusText, totalDuration } from "../logic.js";
  import Cover from "./Cover.svelte";
  import Icon from "./Icon.svelte";

  let { route } = $props();
  const namespace = store.browseKey;
  const current = $derived(namespace !== null && namespace === store.browseKey && store.connection === "open" && !store.invalid);
  const available = $derived(store.snapshot?.import != null);
  const revision = $derived(store.snapshot?.import?.revision ?? null);
  const result = resource(() => (current && available ? query("api/import/album", { id: route.id }) : null), () => store.browseSignal);
  const album = $derived(current && result.data?.album?.id === route.id ? result.data.album : null);
  const tracks = $derived(Array.isArray(album?.tracks) ? album.tracks : []);
  const inLibrary = $derived(current ? result.data?.in_library ?? null : null);
  const libraryLink = $derived(inLibrary && typeof inLibrary.id === "string" && inLibrary.id
    && store.browseItemCurrent({ library: inLibrary.library, namespace })
    ? buildRoute({ view: "album", library: inLibrary.library, id: inLibrary.id, namespace }) : null);

  let adding = $state(false);
  let addError = $state("");
  /** The request returned by adding, with `existing` when someone asked for this album already. */
  let added = $state.raw(null);
  // After adding, each new import revision refreshes the inline status.
  const requests = resource(() => (current && added !== null && revision !== null ? "api/import/requests" : null), () => store.browseSignal, undefined, { keep: true });
  const request = $derived(added === null ? null
    : (Array.isArray(requests.data?.requests) ? requests.data.requests.find((item) => item?.id === added.request.id) : null) ?? added.request);
  const progress = $derived(request === null ? null : requestProgress(request));
  const canAdd = $derived(current && album !== null && !adding && (request === null || request.status === "failed"));

  async function add() {
    if (!canAdd) return;
    adding = true;
    addError = "";
    const id = route.id;
    try {
      const reply = await postForJSON("api/import/requests", { album_id: id });
      if (route.id !== id || !current) return;
      added = { request: reply.request, existing: reply.existing === true };
    } catch (error) {
      addError = error instanceof Error ? error.message : String(error);
    } finally {
      adding = false;
    }
  }
</script>

<section aria-labelledby="add-album-title" aria-busy={result.loading}>
  <div class="section-heading">
    <h1 id="add-album-title">{album?.title ?? "Add music"}</h1>
    <a class="requests-link" href={buildRoute({ view: "requests" })}><Icon name="queue" size={18} /> Requests</a>
  </div>
  {#if !available}
    <p class="notice muted">Adding music is not available on this bot.</p>
  {:else if !current}
    <p class="notice muted" role="status">This page will resume after the connection is restored.</p>
  {:else if result.loading}
    <p class="notice" role="status">Loading album…</p>
  {:else if result.error}
    <div class="notice error" role="alert"><p>{result.error}</p><button class="btn" onclick={() => result.reload()}>Try again</button></div>
  {:else if album}
    <div class="detail-header">
      <div class="detail-art"><Cover importId={album.has_cover ? album.id : null} size={400} /></div>
      <div class="detail-info">
        <p>{album.artist}</p>
        <p class="muted metadata">
          {#if album.year}<span>{album.year}</span>{/if}
          {#if releaseTypeLabel(album.release_type)}<span>{releaseTypeLabel(album.release_type)}</span>{/if}
          <span>{tracks.length} {tracks.length === 1 ? "track" : "tracks"}</span>
          {#if totalDuration(tracks) > 0}<span>{formatDuration(totalDuration(tracks))}</span>{/if}
        </p>
        {#if inLibrary}
          <p class="in-library" id="in-library-hint">Your library already has an album with this name.{#if libraryLink}{" "}<a href={libraryLink}>Open it in the library</a>{/if}</p>
        {/if}
        <div class="detail-actions">
          <button class="btn primary" disabled={!canAdd} aria-describedby={inLibrary ? "in-library-hint" : undefined} onclick={add}><Icon name="add" /> {inLibrary ? "Add anyway" : "Add to library"}</button>
        </div>
        {#if addError}<p class="notice error" role="alert">{addError}</p>{/if}
        <div class="add-status" role="status">
          {#if request}
            {#if added.existing}<p>This album was asked for already.</p>{/if}
            <p class:failed={request.status === "failed"}>{requestStatusText(request)}</p>
            {#if progress}<progress value={progress.value} max={progress.max} aria-label="Tracks added"></progress>{/if}
            <p><a href={buildRoute({ view: "requests" })}>See all requests</a></p>
          {/if}
        </div>
      </div>
    </div>
    <ol class="track-list" aria-label="Tracks">
      {#each tracks as track, i (i)}
        <li>
          <span class="number">{track.position ?? i + 1}</span>
          <div class="description">
            <div class="title" title={track.title}>{track.title}</div>
            {#if track.artist}<div class="secondary muted" title={track.artist}>{track.artist}</div>{/if}
          </div>
          <span class="duration muted">{formatDuration(track.duration)}</span>
        </li>
      {/each}
    </ol>
    {#if !tracks.length}<p class="notice muted">No tracks listed for this album.</p>{/if}
  {/if}
</section>

<style>
  .section-heading { display: flex; align-items: center; justify-content: space-between; gap: 0.5rem 1rem; flex-wrap: wrap; margin-bottom: 1rem; }
  h1 { margin: 0; overflow-wrap: anywhere; }
  .requests-link { display: inline-flex; align-items: center; gap: 0.35rem; white-space: nowrap; }
  .detail-header { display: flex; gap: 1.5rem; align-items: center; margin: 1rem 0 2rem; }
  .detail-art { width: 11rem; flex: none; }
  .detail-info { min-width: 0; overflow-wrap: anywhere; }
  .metadata { display: flex; flex-wrap: wrap; gap: 0.7rem; }
  .in-library { color: var(--warning); }
  .in-library a { text-decoration: underline; }
  .detail-actions { display: flex; flex-wrap: wrap; gap: 0.5rem; }
  .add-status p { margin: 0.5rem 0 0; }
  .add-status a { text-decoration: underline; }
  .add-status .failed { color: var(--error); }
  progress { width: 100%; max-width: 20rem; margin-top: 0.5rem; accent-color: var(--accent); }
  .track-list { list-style: none; padding: 0; margin: 0; }
  li { display: flex; align-items: center; gap: 0.65rem; min-height: 3.6rem; padding: 0.4rem; border-bottom: 1px solid var(--border); border-radius: 4px; }
  li:hover { background: var(--surface-2); }
  .number { flex: none; min-width: 2rem; text-align: right; font-variant-numeric: tabular-nums; color: var(--muted); font-size: 0.85rem; }
  .description { flex: 1; min-width: 0; }
  .title, .secondary { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .secondary { font-size: 0.8rem; }
  .duration { font-variant-numeric: tabular-nums; font-size: 0.85rem; }
  @media (max-width: 600px) { .detail-header { gap: 1rem; align-items: flex-start; } .detail-art { width: 6.5rem; } }
</style>
