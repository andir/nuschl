// SPDX-License-Identifier: EUPL-1.2
// DJ mode controls: commands to `api/dj` and the values a control shows
// while it moves. Continuous controls (faders, knobs) send the latest value
// about 30 times a second at most, one request at a time per control, and
// show their own value until the bot's state has caught up.
import { postJSON } from "./api.js";
import { store } from "./store.svelte.js";
import { DJ_SEND_MS, clamp, paced } from "./logic.js";

/** How long a moved control keeps its own value after its last request. */
const HOLD_MS = 700;

class Dj {
  /** Values of controls being moved, by key (`player:deck:name` or `player:name`). */
  local = $state.raw({});
  /** The length of the one-button fade, in seconds; the page and MIDI share it. */
  fadeSeconds = $state(8);
  #senders = new Map();
  #releases = new Map();

  /** The value a control shows: its own while it moves, otherwise the bot's. */
  value(key, server) {
    return Object.hasOwn(this.local, key) ? this.local[key] : server;
  }

  /** Sends one control; on failure shows the bot's reason and returns false. */
  async send(body) {
    try {
      await postJSON("api/dj", body);
      return true;
    } catch (error) {
      store.toast(error instanceof Error ? error.message : String(error));
      return false;
    }
  }

  #release(key) {
    clearTimeout(this.#releases.get(key));
    this.#releases.set(key, setTimeout(() => {
      if (!this.#senders.get(key)?.idle()) return;
      const local = { ...this.local };
      delete local[key];
      this.local = local;
    }, HOLD_MS));
  }

  #continuous(key, value, body) {
    this.local = { ...this.local, [key]: value };
    let send = this.#senders.get(key);
    if (!send) {
      send = paced(async (body) => {
        await this.send(body);
        this.#release(key);
      }, DJ_SEND_MS);
      this.#senders.set(key, send);
    }
    clearTimeout(this.#releases.get(key));
    send(body);
  }

  setMode(player, on) {
    return this.send({ player, control: "mode", on });
  }

  /** Loads a library song (`{ library, id, namespace }`) or a queue entry (`{ entry }`) onto `deck`. */
  load(player, deck, item) {
    if (item.entry != null) return this.send({ player, control: "load", deck, entry: item.entry });
    if (!store.browseItemCurrent(item)) {
      store.toast("The library changed or is reconnecting. Choose the song from the refreshed library.");
      return Promise.resolve(false);
    }
    return this.send({ player, control: "load", deck, library: item.library, id: item.id });
  }

  /** Loads the queue's next song onto `deck`. */
  loadNext(player, deck) {
    const entry = store.snapshot?.players[player]?.queue[0];
    if (!entry) {
      store.toast("The queue is empty.");
      return Promise.resolve(false);
    }
    return this.load(player, deck, { entry: entry.id });
  }

  eject(player, deck) {
    return this.send({ player, control: "eject", deck });
  }

  play(player, deck, playing) {
    return this.send({ player, control: "play", deck, playing });
  }

  seek(player, deck, ms) {
    return this.send({ player, control: "seek", deck, position_ms: Math.max(0, Math.round(ms)) });
  }

  setCue(player, deck) {
    return this.send({ player, control: "set_cue", deck });
  }

  jumpToCue(player, deck) {
    return this.send({ player, control: "jump_to_cue", deck });
  }

  sync(player, deck) {
    return this.send({ player, control: "sync", deck });
  }

  tempoRange(player, deck, range) {
    return this.send({ player, control: "tempo_range", deck, range });
  }

  volume(player, deck, volume) {
    const value = clamp(Math.round(volume), 0, 100);
    this.#continuous(`${player}:${deck}:volume`, value, { player, control: "volume", deck, volume: value });
  }

  eq(player, deck, band, db) {
    const value = clamp(Math.round(db * 2) / 2, -26, 6);
    this.#continuous(`${player}:${deck}:eq:${band}`, value, { player, control: "eq", deck, band, db: value });
  }

  filter(player, deck, filter) {
    const value = clamp(Math.round(filter * 100) / 100, -1, 1);
    this.#continuous(`${player}:${deck}:filter`, value, { player, control: "filter", deck, value });
  }

  /** `tempo` in percent, within `range`. */
  tempo(player, deck, tempo, range) {
    const value = clamp(Math.round(tempo * 100) / 100, -range, range);
    this.#continuous(`${player}:${deck}:tempo`, value, { player, control: "tempo", deck, tempo: value });
  }

  crossfader(player, position) {
    const value = clamp(Math.round(position * 100) / 100, -1, 1);
    this.#continuous(`${player}:crossfader`, value, { player, control: "crossfader", position: value });
  }

  curve(player, curve) {
    return this.send({ player, control: "curve", curve });
  }

  transition(player, seconds) {
    this.#senders.get(`${player}:crossfader`)?.cancel();
    return this.send({ player, control: "transition", seconds });
  }

  autoLoad(player, on) {
    return this.send({ player, control: "auto_load", on });
  }
}

export const dj = new Dj();
