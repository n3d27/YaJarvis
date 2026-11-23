<script>
  import { icon, toast } from "#helpers/core.js";
  import { speaker } from "#state/panelState.js";
  import {
    music,
    playMusicAction,
    youtubeLibrary,
    youtubePlaylistItems,
  } from "#state/player.js";
  import qStyles from "../PlayerQueue/PlayerQueue.module.css";
  import styles from "./YouTubeLibrary.module.css";

  let tab = $state("liked");
  let library = $state.raw(null);
  let loading = $state(false);
  let loadError = $state("");
  let selectedPlaylist = $state.raw(null);
  let playlistTracks = $state.raw([]);
  let playlistLoading = $state(false);
  let playlistError = $state("");
  let busy = $state(false);
  let lastSignedKey = "";
  let librarySeq = 0;
  let playlistSeq = 0;

  let youtube = $derived($speaker?.youtube);
  let signedIn = $derived(
    Boolean(youtube?.plugin && (youtube?.account?.signed_in || youtube?.set)),
  );
  let accountLabel = $derived(
    [
      youtube?.account?.name && youtube.account.name !== "Аккаунт Google" ? youtube.account.name : "",
      youtube?.account?.handle || youtube?.account?.email || "",
    ]
      .filter((v, i, arr) => v && arr.indexOf(v) === i)
      .join(" · "),
  );
  let signedKey = $derived(
    signedIn
      ? `${Boolean(youtube?.account?.signed_in)}:${Boolean(youtube?.set)}:${youtube?.account?.name || ""}:${youtube?.account?.handle || ""}:${youtube?.account?.email || ""}`
      : "",
  );
  let liked = $derived(library?.liked || []);
  let playlists = $derived(library?.playlists || []);

  $effect(() => {
    const key = signedKey;
    if (!key) {
      lastSignedKey = "";
      library = null;
      loadError = "";
      selectedPlaylist = null;
      playlistTracks = [];
      playlistError = "";
      return;
    }
    if (key !== lastSignedKey) {
      lastSignedKey = key;
      void fetchLibrary(false);
    }
  });

  async function fetchLibrary(refresh = false) {
    if (!signedIn) return;
    const seq = ++librarySeq;
    loading = true;
    loadError = "";
    try {
      const data = await youtubeLibrary(refresh);
      if (seq !== librarySeq) return;
      library = data;
      loadError = data?.error || "";
      if (refresh && selectedPlaylist) {
        void openPlaylist(selectedPlaylist, true);
      }
    } catch (error) {
      if (seq === librarySeq) {
        loadError = error?.message || "Не удалось загрузить медиатеку";
      }
    } finally {
      if (seq === librarySeq) loading = false;
    }
  }

  async function openPlaylist(pl, refresh = false) {
    if (!pl?.ID) return;
    const seq = ++playlistSeq;
    selectedPlaylist = pl;
    playlistTracks = [];
    playlistError = "";
    playlistLoading = true;
    try {
      const data = await youtubePlaylistItems(pl.ID, refresh);
      if (seq !== playlistSeq) return;
      playlistTracks = data?.tracks || [];
    } catch (error) {
      if (seq === playlistSeq) {
        playlistError = error?.message || "Не удалось загрузить плейлист";
      }
    } finally {
      if (seq === playlistSeq) playlistLoading = false;
    }
  }

  async function playAction(body) {
    if (busy) return;
    busy = true;
    try {
      await playMusicAction(body);
    } finally {
      busy = false;
    }
  }

  function queueTrack(track) {
    if (!track?.ID || busy) return;
    busy = true;
    void (async () => {
      try {
        const ok = await music({ action: "queue", query: track.ID });
        if (ok) toast("Добавлено в очередь");
      } finally {
        busy = false;
      }
    })();
  }

  function thumbnail(track) {
    return track?.ID && !/^\d+$/.test(track.ID)
      ? `https://i.ytimg.com/vi/${encodeURIComponent(track.ID)}/mqdefault.jpg`
      : "";
  }
</script>

{#if signedIn}
  <section class={["card", styles.section]} aria-labelledby="yt-library-title">
    <div class="card-head">
      <div>
        <h2 id="yt-library-title">Медиатека YouTube</h2>
        <span class="meta">{accountLabel ? `${accountLabel} · ` : ""}лайки и сохранённые плейлисты</span>
      </div>
      <div class={styles.headRight}>
        <div class="seg sm" role="tablist" aria-label="Разделы медиатеки YouTube">
          <button
            type="button"
            role="tab"
            class={{ on: tab === "liked" }}
            aria-selected={tab === "liked"}
            onclick={() => {
              tab = "liked";
            }}
          >
            Лайки{#if liked.length}<small>{liked.length}</small>{/if}
          </button>
          <button
            type="button"
            role="tab"
            class={{ on: tab === "playlists" }}
            aria-selected={tab === "playlists"}
            onclick={() => {
              tab = "playlists";
            }}
          >
            Плейлисты{#if playlists.length}<small>{playlists.length}</small>{/if}
          </button>
        </div>
        <button
          class={["cbtn", styles.refreshBtn]}
          type="button"
          disabled={loading || playlistLoading}
          data-tip="Обновить медиатеку"
          aria-label="Обновить медиатеку"
          onclick={() => fetchLibrary(true)}
        >
          {@html icon("refresh")}
        </button>
      </div>
    </div>

    {#if loadError}
      <p class={styles.errorNote}>{loadError}</p>
    {/if}

    {#if tab === "liked"}
      {#if loading && !library}
        <p class="empty">Загружаю лайки…</p>
      {:else if liked.length}
        <div class={styles.toolbar}>
          <button
            class={[styles.actionBtn, styles.actionPrimary]}
            type="button"
            disabled={busy}
            onclick={() => playAction({ action: "liked" })}
          >
            {@html icon("play", "solid")}<span>Слушать всё</span>
          </button>
          <button
            class={styles.actionBtn}
            type="button"
            disabled={busy}
            onclick={() => playAction({ action: "liked", shuffle: true })}
          >
            {@html icon("shuffle")}<span>Вперемешку</span>
          </button>
        </div>
        <ul class={[qStyles.list, styles.scrollList]}>
          {#each liked as track, index}
            <li class={qStyles.row}>
              <button
                class={qStyles.track}
                type="button"
                aria-label={`Включить «${track.Title || "…"}»`}
                disabled={busy || !track.ID}
                onclick={() => playAction({ action: "liked", index })}
              >
                <img class={qStyles.image} src={thumbnail(track)} alt="" loading="lazy" />
                <span class={qStyles.description}>
                  <b>{track.Title || "…"}</b>
                  <span>{track.Author || ""}</span>
                </span>
              </button>
              <button
                class={["ib", qStyles.remove]}
                type="button"
                disabled={busy || !track.ID}
                aria-label={`Добавить в очередь «${track.Title || "…"}»`}
                data-tip="В очередь"
                onclick={() => queueTrack(track)}
              >
                {@html icon("plus")}
              </button>
            </li>
          {/each}
        </ul>
      {:else}
        <p class="empty">Понравившиеся видео пока не найдены в аккаунте YouTube</p>
      {/if}
    {:else if selectedPlaylist}
      <div class={styles.toolbar}>
        <button
          class={styles.actionBtn}
          type="button"
          onclick={() => {
            selectedPlaylist = null;
            playlistTracks = [];
            playlistError = "";
          }}
        >
          {@html icon("left")}<span>Все плейлисты</span>
        </button>
        <span class={styles.playlistTitle} title={selectedPlaylist.Title}>
          {selectedPlaylist.Title}
        </span>
        <button
          class={[styles.actionBtn, styles.actionPrimary]}
          type="button"
          disabled={busy}
          onclick={() => playAction({ action: "playlist", id: selectedPlaylist.ID })}
        >
          {@html icon("play", "solid")}<span>Слушать всё</span>
        </button>
        <button
          class={styles.actionBtn}
          type="button"
          disabled={busy}
          onclick={() =>
            playAction({
              action: "playlist",
              id: selectedPlaylist.ID,
              shuffle: true,
            })}
        >
          {@html icon("shuffle")}<span>Вперемешку</span>
        </button>
      </div>
      {#if playlistLoading}
        <p class="empty">Загружаю треки плейлиста…</p>
      {:else if playlistError}
        <p class={styles.errorNote}>{playlistError}</p>
      {:else if playlistTracks.length}
        <ul class={[qStyles.list, styles.scrollList]}>
          {#each playlistTracks as track, index}
            <li class={qStyles.row}>
              <button
                class={qStyles.track}
                type="button"
                aria-label={`Включить «${track.Title || "…"}»`}
                disabled={busy || !track.ID}
                onclick={() =>
                  playAction({
                    action: "playlist",
                    id: selectedPlaylist.ID,
                    index,
                  })}
              >
                <img class={qStyles.image} src={thumbnail(track)} alt="" loading="lazy" />
                <span class={qStyles.description}>
                  <b>{track.Title || "…"}</b>
                  <span>{track.Author || ""}</span>
                </span>
              </button>
              <button
                class={["ib", qStyles.remove]}
                type="button"
                disabled={busy || !track.ID}
                aria-label={`Добавить в очередь «${track.Title || "…"}»`}
                data-tip="В очередь"
                onclick={() => queueTrack(track)}
              >
                {@html icon("plus")}
              </button>
            </li>
          {/each}
        </ul>
      {:else}
        <p class="empty">В этом плейлисте нет доступных видео</p>
      {/if}
    {:else if loading && !library}
      <p class="empty">Загружаю плейлисты…</p>
    {:else if playlists.length}
      <ul class={[qStyles.list, styles.scrollList]}>
        {#each playlists as pl}
          <li class={qStyles.row}>
            <button
              class={qStyles.track}
              type="button"
              aria-label={`Открыть плейлист «${pl.Title}»`}
              onclick={() => openPlaylist(pl)}
            >
              {#if pl.Thumb}
                <img class={qStyles.image} src={pl.Thumb} alt="" loading="lazy" />
              {:else}
                <span class={[qStyles.image, styles.thumbFallback]}>
                  {@html icon("music")}
                </span>
              {/if}
              <span class={qStyles.description}>
                <b>{pl.Title}</b>
                <span>{pl.Count || "Плейлист YouTube"}</span>
              </span>
            </button>
            <button
              class={["cbtn", styles.playlistPlay]}
              type="button"
              disabled={busy}
              aria-label={`Включить плейлист «${pl.Title}»`}
              data-tip="Включить плейлист"
              onclick={() => playAction({ action: "playlist", id: pl.ID })}
            >
              {@html icon("play", "solid")}
            </button>
          </li>
        {/each}
      </ul>
    {:else}
      <p class="empty">Сохранённых плейлистов в аккаунте YouTube пока нет</p>
    {/if}
  </section>
{/if}
