<script>
  import { onMount } from "svelte";
  import { api, icon, toast, upload, waitRestart } from "#helpers/core.js";
  import { renderMarkdown } from "#helpers/markdown.js";
  import styles from "./Updates.module.css";

  const controller = new AbortController();
  const { signal } = controller;
  let update = $state.raw(null);
  let loadError = $state("");
  let pending = $state("");

  let automatic = $derived(!!(update?.check || update?.auto));
  let releasesUrl = $derived(
    update?.repo ? `https://github.com/${update.repo}/releases` : "https://github.com/newfpv/YaJarvis/releases"
  );
  let releaseUrl = $derived(
    update?.repo && update?.latest ? `https://github.com/${update.repo}/releases/tag/v${update.latest}` : releasesUrl
  );
  let cleanNotes = $derived(stripLeadingTitle(update?.notes, update?.latest));

  onMount(() => {
    void load();
    return () => controller.abort();
  });

  function stripLeadingTitle(raw, latest) {
    const text = String(raw ?? "").trim();
    if (!text) return "";
    const lines = text.split(/\r?\n/);
    const first = lines[0]?.trim() || "";
    const ver = String(latest ?? "").trim().replace(/^v/i, "");
    if (/^#{1,3}\s+/.test(first) && ver && first.toLowerCase().includes(ver.toLowerCase())) {
      return lines.slice(1).join("\n").trim();
    }
    return text;
  }

  async function load() {
    try {
      update = await api("GET", "/api/update", undefined, { signal });
    } catch (error) {
      if (!signal.aborted) loadError = error.message;
    }
  }

  async function run(operation, action) {
    if (pending) return;
    pending = operation;
    try {
      await action();
    } catch (error) {
      if (!signal.aborted) toast(error.message, true);
    } finally {
      pending = "";
    }
  }

  function savePreferences(changes, message) {
    void run("preferences", async () => {
      await api("PUT", "/api/update", changes);
      if (signal.aborted) return;
      update = { ...update, ...changes };
      toast(message);
    });
  }

  function setAutomatic(on) {
    savePreferences({ check: on, auto: on }, on ? "Автообновление включено" : "Обновления — вручную");
  }

  function setStats(event) {
    const input = event.currentTarget;
    const stats = input.checked;
    input.checked = update.stats;
    savePreferences({ stats }, stats ? "Счётчик включён" : "Счётчик выключен");
  }

  function checkNow() {
    void run("check", async () => {
      const next = await api("POST", "/api/update/check");
      if (!signal.aborted) update = next;
    });
  }

  function install() {
    void run("install", async () => {
      await api("POST", "/api/update/install");
      await waitRestart("Обновление скачано — Джарвис перезапускается");
    });
  }

  function installFile(event) {
    const input = event.currentTarget;
    const file = input.files[0];
    input.value = "";
    if (!file) return;
    void run("upload", async () => {
      toast("Загружаю на колонку…");
      await upload("PUT", "/api/update/upload", file);
      await waitRestart("Файл принят — Джарвис перезапускается");
    });
  }
</script>

<div class={styles.updates}>
  {#if loadError}
    <p class="err" role="alert">{loadError}</p>
  {:else if update}
    <div class={styles.statusCard}>
      <div class={styles.versionInfo}>
        <span class={styles.eyebrow}>Текущая версия</span>
        <div class={styles.versionRow}>
          <strong class={styles.version}>{update.version}</strong>
          {#if update.busy}
            <span class={[styles.pill, styles.pillBusy]}>
              <span class="spin"></span>
              {update.busy}…
            </span>
          {:else if update.available}
            <span class={[styles.pill, styles.pillAccent]}>Доступна {update.latest}</span>
          {:else if update.latest}
            <span class={[styles.pill, styles.pillOk]}>Актуальная</span>
          {/if}
        </div>
        {#if update.checked}
          <small class={styles.checked}>Проверено {update.checked}</small>
        {/if}
      </div>

      <div class={styles.toolbar}>
        {#if update.repo}
          <button class="btn sm" type="button" disabled={!!pending} onclick={checkNow}>
            {@html icon("refresh")}
            {pending === "check" ? "Проверяю…" : "Проверить"}
          </button>
        {/if}
        <label class={["btn sm ghost", styles.file]} data-tip={`Поставить файл ${update.asset} с компьютера`}>
          {@html icon("upload")}
          Файлом…
          <input type="file" disabled={!!pending} onchange={installFile}>
        </label>
        <a
          class="btn sm ghost"
          href={releasesUrl}
          target="_blank"
          rel="noopener noreferrer"
          data-tip="Все релизы на GitHub"
        >
          GitHub
          {@html icon("ext")}
        </a>
      </div>
    </div>

    {#if update.error}
      <p class={styles.errorBanner} role="alert">{update.error}</p>
    {/if}

    {#if update.available}
      <div class={styles.releaseCard}>
        <div class={styles.releaseHead}>
          <div class={styles.releaseTitle}>
            <span class={styles.releaseIcon} aria-hidden="true">{@html icon("bolt")}</span>
            <div class={styles.releaseCopy}>
              <b>Доступна версия {update.latest}</b>
              <small>
                Обновление по воздуху ·
                <a class={styles.releaseLink} href={releaseUrl} target="_blank" rel="noopener noreferrer">
                  Страница релиза
                  {@html icon("ext")}
                </a>
              </small>
            </div>
          </div>
          <button class="btn sm primary" type="button" disabled={!!pending} onclick={install}>
            {pending === "install" ? "Скачиваю…" : `Обновить до ${update.latest}`}
          </button>
        </div>

        {#if cleanNotes}
          <div class={styles.changelog}>
            <span class={styles.changelogLabel}>Что нового</span>
            <div class={styles.notes}>{@html renderMarkdown(cleanNotes)}</div>
          </div>
        {/if}
      </div>
    {:else if cleanNotes && update.latest}
      <details class={["mini", styles.currentNotes]}>
        <summary>Что нового в {update.latest}</summary>
        <div class={styles.notes}>{@html renderMarkdown(cleanNotes)}</div>
      </details>
    {/if}

    <div class={styles.prefs}>
      <div class={styles.prefRow}>
        <div class={styles.prefCopy}>
          <b>Режим обновлений</b>
          <small>
            {#if automatic}
              Прошивка и плагины обновляются сами с 3 до 6 утра, когда колонка молчит
            {:else if !update.repo}
              Укажите репозиторий обновлений или выберите файл с компьютера
            {:else}
              Прошивка и плагины обновляются вручную
            {/if}
          </small>
        </div>
        <div class="seg" role="group" aria-label="Режим обновлений">
          <button
            type="button"
            class:on={!automatic}
            aria-pressed={!automatic}
            disabled={!!pending}
            onclick={() => setAutomatic(false)}
          >Вручную</button>
          <button
            type="button"
            class:on={automatic}
            aria-pressed={automatic}
            disabled={!!pending}
            onclick={() => setAutomatic(true)}
          >Автообновление</button>
        </div>
      </div>

      <div class={styles.prefDivider}></div>

      <label class={["sw hu", styles.prefSwitch]}>
        <input
          type="checkbox"
          checked={update.stats}
          disabled={!!pending}
          onchange={setStats}
          aria-label="Счётчик установок"
          aria-describedby="update-stats-description"
        >
        <span class="tr"></span>
        <span class="hu-t">
          <b>Счётчик установок</b>
          <small id="update-stats-description">
            Раз на версию колонка скачивает пустой файл с GitHub — так в README видно, сколько колонок с Джарвисом. Передаются только модель и версия.
          </small>
        </span>
      </label>
    </div>
  {:else}
    <p class="note">…</p>
  {/if}
</div>
