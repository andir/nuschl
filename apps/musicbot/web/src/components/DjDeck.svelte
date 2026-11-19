<!-- SPDX-License-Identifier: EUPL-1.2 -->
<script>
  import { store } from "../store.svelte.js";
  import { dj } from "../dj.svelte.js";
  import { cueListening } from "../listening.svelte.js";
  import { DJ_EQ_MAX, DJ_EQ_MIN, DJ_TEMPO_RANGES, buildRoute, deckBpm, deckPosition, decibelText, filterText, formatDuration, tempoText } from "../logic.js";
  import Cover from "./Cover.svelte";
  import Icon from "./Icon.svelte";
  import DjSlider from "./DjSlider.svelte";

  /** `data` is the deck from the state; `learning` names the control MIDI learn waits for. */
  let { player, deck, data, learning = null } = $props();
  const letter = $derived(deck === 0 ? "A" : "B");
  const id = $props.id();
  const track = $derived(data.entry?.track ?? null);
  const key = (name) => `${player}:${deck}:${name}`;
  const volume = $derived(dj.value(key("volume"), data.volume));
  const tempo = $derived(dj.value(key("tempo"), data.tempo));
  const filter = $derived(dj.value(key("filter"), data.filter));
  const bands = $derived([
    { band: "high", label: "High", value: dj.value(key("eq:high"), data.eq.high) },
    { band: "mid", label: "Mid", value: dj.value(key("eq:mid"), data.eq.mid) },
    { band: "low", label: "Low", value: dj.value(key("eq:low"), data.eq.low) },
  ]);
  const bpm = $derived(deckBpm(data.bpm, tempo));
  const duration = $derived(track?.duration ?? null);
  const max = $derived(typeof duration === "number" && duration > 0 ? duration * 1000 : 0);
  const cueing = $derived(cueListening.deck === deck && cueListening.player === player);

  let now = $state(performance.now());
  let seeking = $state(null);
  $effect(() => {
    if (!data.advancing) return;
    let frame = requestAnimationFrame(function tick() {
      now = performance.now();
      frame = requestAnimationFrame(tick);
    });
    return () => cancelAnimationFrame(frame);
  });
  const position = $derived(seeking ?? deckPosition(data.position_ms, data.advancing, tempo, store.receivedAt, now, duration));
  const learnClass = (control) => learning === control;

  function seek(ms) {
    seeking = null;
    if (track) dj.seek(player, deck, ms);
  }
</script>

<article class="deck card" aria-labelledby="{id}-title">
  <header class="deck-head">
    <span class="deck-letter" aria-hidden="true">{letter}</span>
    <Cover library={track?.library ?? 0} art={track?.cover_art ?? null} size={128} px={56} />
    <div class="deck-text">
      <h2 id="{id}-title" class="visually-hidden">Deck {letter}</h2>
      {#if track}
        <div class="deck-title" title={track.title}><a href={buildRoute({ view: "song", library: track.library, id: track.id, namespace: store.browseKey })}>{track.title}</a></div>
        <div class="deck-sub muted">{track.artist ?? "Unknown artist"}</div>
        <div class="deck-sub muted small">Added by {data.entry.added_by}</div>
      {:else}
        <div class="deck-sub muted">Deck {letter} is empty. Load a song from the library, a playlist or the queue.</div>
      {/if}
    </div>
    <div class="bpm" title={data.bpm ? `${data.bpm} BPM in the library` : "BPM unknown: sync needs the library's BPM of both songs"}>
      <span class="bpm-value num">{bpm === null ? "—" : bpm.toFixed(1)}</span>
      <span class="eyebrow">BPM</span>
    </div>
  </header>

  {#if data.error}<p class="deck-error small" role="alert">{data.error}</p>{/if}

  <div class="time">
    <span class="num">{formatDuration(position / 1000) || "0:00"}</span>
    <input
      type="range"
      min="0"
      max={max || 1}
      step="100"
      value={Math.min(position, max || 1)}
      disabled={!track || !max}
      aria-label="Position on deck {letter}"
      aria-valuetext="{formatDuration(position / 1000) || '0:00'} of {formatDuration(duration) || 'unknown length'}"
      oninput={(event) => (seeking = Number(event.currentTarget.value))}
      onchange={(event) => seek(Number(event.currentTarget.value))}
      onblur={() => (seeking = null)}
    />
    <span class="num muted">{max ? `−${formatDuration(Math.max(0, max - position) / 1000) || "0:00"}` : "—"}</span>
  </div>
  <div class="deck-status small muted" role="status">
    {#if data.buffering && data.playing}Loading…{:else if track}Cue at {formatDuration(data.cue_ms / 1000) || "0:00"}{/if}
  </div>

  <div class="transport">
    <button class="play" class:learning={learnClass(`${deck}.play`)} data-control="{deck}.play" disabled={!track} aria-label="{data.playing ? 'Pause' : 'Play'} deck {letter}" aria-pressed={data.playing} onclick={() => dj.play(player, deck, !data.playing)}>
      <Icon name={data.playing ? "pause" : "play"} size={22} />
    </button>
    <button class="btn" class:learning={learnClass(`${deck}.cue`)} data-control="{deck}.cue" disabled={!track} title="Go to the cue point" onclick={() => dj.jumpToCue(player, deck)}><Icon name="cue" size={16} />Cue</button>
    <button class="btn" class:learning={learnClass(`${deck}.setCue`)} data-control="{deck}.setCue" disabled={!track} title="Set the cue point here" onclick={() => dj.setCue(player, deck)}>Set cue</button>
    <button class="icon-btn" class:active={cueing} disabled={!track && !cueing} aria-pressed={cueing} aria-label="Hear deck {letter} on headphones in this browser" title={cueing ? "Stop the headphone cue" : "Hear this deck before its fader, only in this browser"} onclick={() => cueListening.toggle(player, deck)}><Icon name="headphones" /></button>
    <button class="icon-btn" class:learning={learnClass(`${deck}.loadNext`)} data-control="{deck}.loadNext" aria-label="Load the queue's next song onto deck {letter}" title="Load next from the queue" onclick={() => dj.loadNext(player, deck)}><Icon name="next" /></button>
    <button class="icon-btn" class:learning={learnClass(`${deck}.eject`)} data-control="{deck}.eject" disabled={!track} aria-label="Eject deck {letter}" title="Eject" onclick={() => dj.eject(player, deck)}><Icon name="eject" /></button>
  </div>

  <div class="strip">
    <div class="eq" role="group" aria-label="EQ and filter of deck {letter}">
      {#each bands as band (band.band)}
        <DjSlider vertical label={band.label} value={band.value} min={DJ_EQ_MIN} max={DJ_EQ_MAX} step="0.5" reset={0} text={decibelText(band.value)} shown={decibelText(band.value).replace(" dB", "").replace(", cut", "")}
          control="{deck}.eq.{band.band}" learning={learnClass(`${deck}.eq.${band.band}`)} onchange={(db) => dj.eq(player, deck, band.band, db)} />
      {/each}
      <DjSlider vertical label="Filter" value={filter} min={-1} max={1} step="0.01" reset={0} text={filterText(filter)} shown={filterText(filter).replace("Low-pass", "LP").replace("High-pass", "HP").replace(" %", "")}
        control="{deck}.filter" learning={learnClass(`${deck}.filter`)} onchange={(value) => dj.filter(player, deck, value)} />
    </div>
    <div class="fader">
      <DjSlider vertical label="Volume" value={volume} min={0} max={100} step="1" text="{volume} %"
        control="{deck}.volume" learning={learnClass(`${deck}.volume`)} onchange={(value) => dj.volume(player, deck, value)} />
    </div>
    <div class="tempo" role="group" aria-label="Tempo of deck {letter}">
      <DjSlider vertical label="Tempo" value={tempo} min={-data.tempo_range} max={data.tempo_range} step="0.05" reset={0} text={tempoText(tempo)}
        control="{deck}.tempo" learning={learnClass(`${deck}.tempo`)} onchange={(value) => dj.tempo(player, deck, value, data.tempo_range)} />
      <div class="range" class:learning={learnClass(`${deck}.range`)} data-control="{deck}.range" role="radiogroup" aria-label="Tempo range of deck {letter}">
        {#each DJ_TEMPO_RANGES as range (range)}
          <button class="chip" role="radio" aria-checked={data.tempo_range === range} aria-label="±{range} %" onclick={() => dj.tempoRange(player, deck, range)}>±{range}</button>
        {/each}
      </div>
      <button class="btn sync" class:learning={learnClass(`${deck}.sync`)} data-control="{deck}.sync" disabled={!data.bpm} title="Match the other deck's BPM" onclick={() => dj.sync(player, deck)}><Icon name="sync" size={16} />Sync</button>
    </div>
  </div>
</article>

<style>
  .deck { display: grid; gap: 0.6rem; min-width: 0; padding: 0.9rem; }
  .deck-head { display: flex; align-items: center; gap: 0.75rem; min-width: 0; }
  .deck-letter { display: grid; place-items: center; flex: none; width: 1.8rem; height: 1.8rem; border-radius: 50%; background: var(--accent-soft); color: var(--accent); font-weight: 700; }
  .deck-text { flex: 1; min-width: 0; line-height: 1.3; }
  .deck-title { font-weight: 600; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .deck-sub { font-size: 0.85rem; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .deck-sub.small { font-size: 0.75rem; }
  .bpm { display: grid; justify-items: end; flex: none; }
  .bpm-value { font-size: 1.35rem; font-weight: 700; line-height: 1.1; }
  .deck-error { margin: 0; color: var(--error); overflow-wrap: anywhere; }
  .time { display: grid; grid-template-columns: auto minmax(0, 1fr) auto; align-items: center; gap: 0.6rem; font-size: 0.85rem; }
  .time input { width: 100%; margin: 0; }
  .deck-status { min-height: 1.1rem; margin-top: -0.35rem; text-align: center; }
  .transport { display: flex; flex-wrap: wrap; align-items: center; justify-content: center; gap: 0.4rem; }
  .play { width: 2.8rem; height: 2.8rem; display: grid; place-items: center; border: none; border-radius: 50%; background: var(--accent); color: var(--accent-fg); }
  .play:hover:not(:disabled) { filter: brightness(1.08); }
  .play:disabled { opacity: 0.4; }
  .transport .btn { padding: 0.4rem 0.75rem; }
  .strip { display: grid; grid-template-columns: minmax(0, 4fr) minmax(3rem, 1fr) minmax(5.5rem, 1.5fr); gap: 0.5rem; align-items: start; padding-top: 0.5rem; border-top: 1px solid var(--border); }
  .eq { display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: 0.15rem; }
  .fader { display: grid; justify-items: center; }
  .tempo { display: grid; justify-items: center; gap: 0.4rem; }
  .range { display: flex; flex-wrap: wrap; justify-content: center; gap: 0.25rem; }
  .chip { padding: 0.15rem 0.45rem; border: 1px solid var(--border); border-radius: 999px; background: var(--surface); color: var(--muted); font-size: 0.75rem; white-space: nowrap; }
  .chip[aria-checked="true"] { border-color: var(--accent); background: var(--accent-soft); color: var(--accent); }
  .sync { padding: 0.3rem 0.6rem; font-size: 0.85rem; }
  .learning { outline: 2px dashed var(--accent); outline-offset: 3px; }
  @media (max-width: 480px) {
    .strip { grid-template-columns: minmax(0, 1fr) minmax(0, 1fr); }
    .eq { grid-column: 1 / -1; }
  }
</style>
