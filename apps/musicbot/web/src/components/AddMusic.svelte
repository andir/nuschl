<!-- SPDX-License-Identifier: EUPL-1.2 -->
<script>
  import { untrack } from "svelte";
  import { store } from "../store.svelte.js";
  import { resource } from "../resource.svelte.js";
  import { query } from "../api.js";
  import { buildRoute, releaseTypeLabel } from "../logic.js";
  import Cover from "./Cover.svelte";
  import Icon from "./Icon.svelte";

  let { route } = $props();
  const namespace = store.browseKey;
  const current = $derived(namespace !== null && namespace === store.browseKey && store.connection === "open" && !store.invalid);
  const available = $derived(store.snapshot?.import != null);
  let search = $state(untrack(() => route.q));
  const result = resource(() => (current && available && route.q.trim() ? query("api/import/search", { q: route.q }) : null), () => store.browseSignal);
  const albums = $derived(current && Array.isArray(result.data?.albums) ? result.data.albums : []);

  function submit(event) {
    event.preventDefault();
    store.navigate({ view: "add", q: search.trim() });
  }
</script>

<section aria-labelledby="add-title" aria-busy={result.loading}>
  <div class="section-heading">
    <h1 id="add-title">Add music</h1>
    <a class="requests-link" href={buildRoute({ view: "requests" })}><Icon name="queue" size={18} /> Requests</a>
  </div>
  {#if !available}
    <p class="notice muted">Adding music is not available on this bot.</p>
  {:else}
    <form class="add-search" role="search" onsubmit={submit}>
      <label for="add-search-input" class="visually-hidden">Search for albums to add</label>
      <input id="add-search-input" type="search" placeholder="Artist or album" maxlength="200" bind:value={search} autocomplete="off" />
      <button class="btn primary" type="submit" disabled={!current}><Icon name="search" size={18} /> Search</button>
    </form>
    {#if !current}
      <p class="notice muted" role="status">Searching will resume after the connection is restored.</p>
    {:else if !route.q.trim()}
      <p class="notice muted">Search by artist, album title or both.</p>
    {:else if result.loading}
      <p class="notice" role="status">Searching…</p>
    {:else if result.error}
      <div class="notice error" role="alert"><p>{result.error}</p><button class="btn" onclick={() => result.reload()}>Try again</button></div>
    {:else if result.data}
      <p class="muted">Results for “{route.q}”</p>
      {#if albums.length}
        <div class="media-grid">
          {#each albums as album (album.id)}
            {@const href = buildRoute({ view: "addAlbum", id: album.id })}
            <article class="media-card">
              <a class="art-link" {href} aria-label="Open {album.title}"><Cover importId={album.has_cover ? album.id : null} /></a>
              <a class="name" {href} title={album.title}>{album.title}</a>
              <div class="muted subtitle" title={album.artist}>{album.artist}</div>
              <div class="muted metadata">
                {#if album.year}<span>{album.year}</span>{/if}
                {#if album.track_count != null}<span>{album.track_count} {album.track_count === 1 ? "track" : "tracks"}</span>{/if}
                {#if releaseTypeLabel(album.release_type)}<span>{releaseTypeLabel(album.release_type)}</span>{/if}
              </div>
            </article>
          {/each}
        </div>
      {:else}
        <p class="notice muted">No albums found.</p>
      {/if}
    {/if}
  {/if}
</section>

<style>
  .section-heading { display: flex; align-items: center; justify-content: space-between; gap: 0.5rem 1rem; flex-wrap: wrap; margin-bottom: 1rem; }
  h1 { margin: 0; overflow-wrap: anywhere; }
  .requests-link { display: inline-flex; align-items: center; gap: 0.35rem; white-space: nowrap; }
  .add-search { display: flex; gap: 0.5rem; margin: 0 0 1.25rem; max-width: 36rem; }
  .add-search input { flex: 1; min-width: 0; }
  .media-card { min-width: 0; display: flex; flex-direction: column; gap: 0.25rem; padding: 0.65rem; border-radius: 8px; background: var(--surface); }
  .media-card:hover, .media-card:focus-within { background: var(--surface-2); }
  .art-link { display: block; margin-bottom: 0.35rem; }
  .name { font-weight: 600; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .subtitle { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 0.85rem; }
  .metadata { display: flex; flex-wrap: wrap; gap: 0.4rem; font-size: 0.75rem; }
</style>
