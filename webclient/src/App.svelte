<script lang="ts">
  import { onMount, tick } from 'svelte';
  import {
    mpState,
    checkJoinLink,
    submitLink,
    enterRoom,
    backToJoin,
    leaveRoom,
    sendChat,
    requestGeneration,
  } from './multiplayer.svelte';

  let linkInput = $state('');
  let nameInput = $state('');
  let chatInput = $state('');
  let logEl: HTMLElement | undefined = $state();
  let composerEl: HTMLTextAreaElement | undefined = $state();

  onMount(() => checkJoinLink());

  const locked = $derived(mpState.lockedBy !== null);
  const closed = $derived(mpState.closedReason !== '');

  const closedText: Record<string, string> = {
    host_left: 'Der Host hat den Raum geschlossen.',
    expired: 'Der Raum ist abgelaufen.',
    not_found: 'Diesen Raum gibt es nicht (mehr). Prüfe den Link.',
    idle: 'Verbindung wegen Inaktivität getrennt.',
    slow: 'Verbindung war zu langsam und wurde getrennt.',
    error: 'Verbindung fehlgeschlagen. Ist der Host online?',
    left: 'Du hast den Raum verlassen.',
  };

  const errorText: Record<string, string> = {
    invalid_link: 'Das sieht nicht wie ein Einladungslink aus.',
    missing_key: 'Das ist nur ein Raumcode. Du brauchst den vollständigen Link — er enthält den Schlüssel zum Entschlüsseln.',
  };

  $effect(() => {
    mpState.messages.length;
    mpState.messages.at(-1)?.text;
    tick().then(() => logEl?.scrollTo({ top: logEl.scrollHeight }));
  });

  function formatTime(timestamp: number): string {
    if (!timestamp) return '';
    return new Intl.DateTimeFormat('de-DE', { hour: '2-digit', minute: '2-digit' }).format(timestamp);
  }

  function resizeComposer() {
    if (!composerEl) return;
    composerEl.style.height = 'auto';
    composerEl.style.height = `${Math.min(composerEl.scrollHeight, 160)}px`;
  }

  async function submitChat() {
    const text = chatInput;
    if (!text.trim() || locked) return;
    chatInput = '';
    await sendChat(text);
    await tick();
    resizeComposer();
    composerEl?.focus();
  }

  function handleComposerKeydown(event: KeyboardEvent) {
    if (event.key === 'Enter' && !event.shiftKey) {
      event.preventDefault();
      void submitChat();
    }
  }

  function leave() {
    leaveRoom();
    backToJoin();
  }
</script>

{#if mpState.view === 'join'}
  <main class="gate">
    <section class="gate-card" aria-labelledby="join-title">
      <div class="brand-mark" aria-hidden="true">R</div>
      <p class="eyebrow">Ryokan Multiplayer</p>
      <h1 id="join-title">Gemeinsam Geschichten erleben</h1>
      <p class="lead">
        Füge den Einladungslink ein, den dir der Host geschickt hat. Der Schlüssel bleibt
        in deinem Browser und eure Unterhaltung ist Ende-zu-Ende-verschlüsselt.
      </p>
      <form class="gate-form" onsubmit={(event) => { event.preventDefault(); submitLink(linkInput); }}>
        <label for="invite-link">Einladungslink</label>
        <input id="invite-link" type="url" bind:value={linkInput} placeholder="https://…/?mp=ABC123#k=…" autocomplete="off" spellcheck="false" />
        <button class="primary-button" type="submit" disabled={!linkInput.trim()}>
          <span>Weiter</span>
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.3" stroke-linecap="round" aria-hidden="true"><path d="M5 12h14M13 6l6 6-6 6" /></svg>
        </button>
      </form>
      {#if mpState.error}<p class="error" role="alert">{errorText[mpState.error]}</p>{/if}
      <p class="privacy-note">
        <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><rect x="4" y="10" width="16" height="11" rx="2"/><path d="M8 10V7a4 4 0 0 1 8 0v3"/></svg>
        Verschlüsselt, bevor etwas dein Gerät verlässt
      </p>
    </section>
  </main>
{:else if mpState.view === 'name'}
  <main class="gate">
    <section class="gate-card compact" aria-labelledby="name-title">
      <div class="room-badge" aria-hidden="true">
        <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2"/><circle cx="9" cy="7" r="4"/><path d="M22 21v-2a4 4 0 0 0-3-3.87M16 3.13a4 4 0 0 1 0 7.75"/></svg>
      </div>
      <p class="eyebrow">Raum {mpState.roomId || '·'}</p>
      <h1 id="name-title">Wie sollen dich die anderen sehen?</h1>
      <p class="lead">Dieser Name wird neben deinen Nachrichten angezeigt.</p>
      <form class="gate-form" onsubmit={(event) => { event.preventDefault(); if (nameInput.trim()) enterRoom(nameInput); }}>
        <label for="display-name">Dein Name</label>
        <input id="display-name" type="text" bind:value={nameInput} placeholder="Name eingeben" maxlength="32" autocomplete="nickname" />
        <button class="primary-button" type="submit" disabled={!nameInput.trim() || mpState.connecting}>{mpState.connecting ? 'Verbinde …' : 'Raum beitreten'}</button>
      </form>
      {#if closed}<p class="error" role="alert">{closedText[mpState.closedReason]}</p>{/if}
      <button class="text-button" type="button" onclick={backToJoin}>
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.3" stroke-linecap="round" aria-hidden="true"><path d="M19 12H5M11 18l-6-6 6-6"/></svg>
        Zurück
      </button>
    </section>
  </main>
{:else}
  <main class="room">
    <header class="room-header">
      <div class="header-inner">
        <div class="character-summary">
          <span class="header-avatar {mpState.sessionCharacter?.color ?? ''}">
            {#if mpState.sessionCharacter?.avatarUrl}<img src={mpState.sessionCharacter.avatarUrl} alt={mpState.sessionCharacter.name} />{:else}{mpState.sessionCharacter?.initials ?? mpState.characterName?.[0]?.toUpperCase() ?? '?'}{/if}
          </span>
          <div class="character-copy">
            <strong>{mpState.sessionCharacter?.name || mpState.characterName || 'Multiplayer'}</strong>
            <span class="participant-line"><i aria-hidden="true"></i>{mpState.count} {mpState.count === 1 ? 'Person' : 'Personen'} im Raum</span>
          </div>
        </div>
        <div class="header-actions">
          <span class="encrypted-label">
            <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><rect x="4" y="10" width="16" height="11" rx="2"/><path d="M8 10V7a4 4 0 0 1 8 0v3"/></svg>
            <span>Ende-zu-Ende-verschlüsselt</span>
          </span>
          <button class="leave-button" type="button" onclick={leave} aria-label="Raum verlassen">
            <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4"/><path d="m16 17 5-5-5-5M21 12H9"/></svg>
            <span>Verlassen</span>
          </button>
        </div>
      </div>
    </header>

    <section class="transcript" bind:this={logEl} aria-live="polite">
      <div class="messages">
        {#each mpState.messages as message (message.id)}
          {#if message.kind === 'system'}
            <p class="system-message">{message.text}</p>
          {:else if message.kind === 'llm'}
            <article class="message ai-message">
              <span class="message-avatar {mpState.sessionCharacter?.color ?? ''}" aria-hidden="true">
                {#if mpState.sessionCharacter?.avatarUrl}<img src={mpState.sessionCharacter.avatarUrl} alt="" />{:else}{mpState.sessionCharacter?.initials ?? message.author[0]?.toUpperCase() ?? 'A'}{/if}
              </span>
              <div class="message-body">
                <div class="message-meta ai-meta"><strong>{message.author}</strong><time datetime={new Date(message.ts).toISOString()}>{formatTime(message.ts)}</time></div>
                <p>{message.text}{#if message.streaming}<span class="stream-dots" aria-label="Antwort wird generiert"><i></i><i></i><i></i></span>{/if}</p>
              </div>
            </article>
          {:else}
            <article class="message participant-message" class:mine={message.mine || message.author === mpState.displayName}>
              <div class="participant-body">
                <div class="message-meta"><strong>{message.author}</strong><time datetime={new Date(message.ts).toISOString()}>{formatTime(message.ts)}</time></div>
                <p>{message.text}</p>
              </div>
            </article>
          {/if}
        {/each}
        {#if mpState.messages.length === 0}
          <div class="empty-state">
            <span aria-hidden="true"><svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8"><path d="M21 15a4 4 0 0 1-4 4H8l-5 3V7a4 4 0 0 1 4-4h10a4 4 0 0 1 4 4z"/></svg></span>
            <p>Noch ist es still</p>
            <small>Schreib etwas oder warte, bis der Host die Geschichte beginnt.</small>
          </div>
        {/if}
      </div>
    </section>

    {#if closed}
      <div class="closed-bar" role="alert"><span>{closedText[mpState.closedReason]}</span><button class="text-button" type="button" onclick={backToJoin}>Neuen Link eingeben</button></div>
    {:else}
      <form class="composer-shell" onsubmit={(event) => { event.preventDefault(); void submitChat(); }}>
        <div class="composer-wrap">
          <div class="composer">
            <textarea bind:this={composerEl} bind:value={chatInput} placeholder={locked ? 'Kurz warten …' : 'Nachricht schreiben …'} disabled={locked} maxlength="4000" rows="1" aria-label="Nachricht" oninput={resizeComposer} onkeydown={handleComposerKeydown}></textarea>
            <div class="composer-toolbar">
              <span class:active-lock={locked} class="room-status">
                {#if locked}<span class="typing-dot" aria-hidden="true"></span>{mpState.characterName || 'Der Charakter'} schreibt gerade …{:else}{mpState.count} {mpState.count === 1 ? 'Person' : 'Personen'} im Raum{/if}
              </span>
              <div class="composer-actions">
                {#if mpState.everyoneCanGenerate}
                  <button class="generate-button" type="button" onclick={requestGeneration} disabled={locked} title="Den Charakter antworten lassen">
                    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M12 3l1.9 5.7L19.6 10l-5.7 1.9L12 17.6l-1.9-5.7L4.4 10l5.7-1.9z"/></svg><span>Jetzt antworten</span>
                  </button>
                {/if}
                <button class="send-button" type="submit" disabled={locked || !chatInput.trim()} aria-label="Nachricht senden"><svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M12 19V5M5 12l7-7 7 7"/></svg></button>
              </div>
            </div>
          </div>
        </div>
      </form>
    {/if}
  </main>
{/if}
