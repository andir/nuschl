<!-- SPDX-License-Identifier: EUPL-1.2 -->
<script>
  import { store } from "../store.svelte.js";
  import { dj } from "../dj.svelte.js";
  import Icon from "./Icon.svelte";
  import DjDeck from "./DjDeck.svelte";
  import DjMixer from "./DjMixer.svelte";
  import MidiPanel from "./MidiPanel.svelte";
  import { midi } from "../midi.svelte.js";

  const learning = $derived(midi.learning);
  const selected = $derived(store.selected);
  const player = $derived(store.player);
  const players = $derived(store.snapshot?.players ?? []);
  const mix = $derived(player?.dj ?? null);
  let switching = $state(false);

  // In learn mode a press on a control picks it for MIDI learn and leaves its value as it is.
  function pick(event) {
    if (!midi.learnMode) return;
    const control = event.target.closest?.("[data-control]");
    if (!control) return;
    event.preventDefault();
    event.stopPropagation();
    if (event.type === "pointerdown") midi.learn(control.dataset.control);
  }

  async function toggle() {
    switching = true;
    await dj.setMode(selected, !mix);
    switching = false;
  }
</script>

<section class="dj-panel" aria-labelledby="dj-title">
  <div class="section-heading">
    <h1 id="dj-title">DJ mode</h1>
    <div class="heading-actions">
      {#if players.length > 1}
        <label><span class="visually-hidden">Queue</span>
          <select value={selected} onchange={(event) => store.selectQueue(Number(event.currentTarget.value))}>
            {#each players as p, i (i)}<option value={i}>{p.name || `Queue ${i + 1}`}</option>{/each}
          </select>
        </label>
      {/if}
      {#if player}
        <button class="btn" class:primary={!mix} disabled={switching} onclick={toggle}><Icon name="decks" size={18} />{mix ? "Leave DJ mode" : "Start DJ mode"}</button>
      {/if}
    </div>
  </div>

  {#if !player}
    <p class="notice muted">No playback queues are configured.</p>
  {:else if !mix}
    <div class="intro card">
      <p>DJ mode mixes two decks into this queue's output: everybody in its Mumble channels and every browser listener hears the mix. The playing song continues on deck A where it is; the queue's next song waits on deck B.</p>
      <p class="muted">Each deck has play, cue point, seek, tempo with BPM sync, a 3-band EQ, a filter and a channel fader; the mixer has a crossfader and a one-button fade to the other deck. Everyone with a link controls the decks. Controls take effect after the network delay, about 0.1 to 0.3 seconds.</p>
    </div>
  {:else}
    {#if mix.notice}<p class="notice error" role="alert">{mix.notice}</p>{/if}
    <div class="decks" class:learn-mode={midi.learnMode} onpointerdowncapture={pick} onclickcapture={pick}>
      <DjDeck player={selected} deck={0} data={mix.decks[0]} {learning} />
      <DjMixer player={selected} {mix} {learning} />
      <DjDeck player={selected} deck={1} data={mix.decks[1]} {learning} />
    </div>
    <p class="muted small">Load songs with the A and B buttons in the library, playlists and the queue. Double-click a fader or knob to return it to its middle.</p>
    <MidiPanel />
  {/if}
</section>

<style>
  .heading-actions { display: flex; align-items: center; gap: 0.5rem; flex-wrap: wrap; }
  .intro { padding: 1rem 1.25rem; max-width: 46rem; }
  .intro p:first-child { margin-top: 0; }
  .decks { display: grid; grid-template-columns: minmax(0, 1fr) minmax(13rem, 16rem) minmax(0, 1fr); gap: 1rem; align-items: start; }
  .notice.error { margin-bottom: 1rem; }
  .learn-mode :global([data-control]) { outline: 1px dashed color-mix(in srgb, var(--accent) 60%, transparent); outline-offset: 2px; cursor: crosshair; }
  .learn-mode :global([data-control].learning) { outline: 2px solid var(--accent); background: var(--accent-soft); }
  @media (max-width: 1250px) {
    .decks { grid-template-columns: minmax(0, 1fr) minmax(0, 1fr); }
    .decks > :global(.mixer) { grid-column: 1 / -1; grid-row: 2; }
  }
  @media (max-width: 760px) {
    .decks { grid-template-columns: minmax(0, 1fr); }
    .decks > :global(.mixer) { grid-row: auto; }
  }
</style>
