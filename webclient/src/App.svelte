<script lang="ts">
  import { onMount, tick } from 'svelte';
  import LanguageSwitcher from './LanguageSwitcher.svelte';
  import { localeState } from './i18n.svelte';
  import * as m from './paraglide/messages.js';
  import type { ClosedReason, JoinError, MpSystemMessage } from './multiplayer.svelte';
  import {
    mpState,
    checkJoinLink,
    submitLink,
    enterRoom,
    backToJoin,
    leaveRoom,
    sendChat,
    sendTyping,
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

  const copy = $derived.by(() => {
    const locale = localeState.current;
    const options = { locale };
    return {
      joinTitle: m.join_title({}, options),
      joinIntro: m.join_intro({}, options),
      invitationLink: m.invitation_link({}, options),
      invitationLinkPlaceholder: m.invitation_link_placeholder({}, options),
      continueButton: m.continue_button({}, options),
      encryptedBeforeLeaving: m.encrypted_before_leaving({}, options),
      roomLabel: (roomId: string) => m.room_label({ roomId }, options),
      nameTitle: m.name_title({}, options),
      nameIntro: m.name_intro({}, options),
      yourName: m.your_name({}, options),
      namePlaceholder: m.name_placeholder({}, options),
      connecting: m.connecting({}, options),
      joinRoom: m.join_room({}, options),
      back: m.back({}, options),
      participantsInRoom: (count: number) => m.participants_in_room({ count }, options),
      endToEndEncrypted: m.end_to_end_encrypted({}, options),
      leaveRoom: m.leave_room({}, options),
      leave: m.leave({}, options),
      responseIsBeingGenerated: m.response_is_being_generated({}, options),
      emptyTitle: m.empty_title({}, options),
      emptyDescription: m.empty_description({}, options),
      enterNewLink: m.enter_new_link({}, options),
      waitBriefly: m.wait_briefly({}, options),
      messagePlaceholder: m.message_placeholder({}, options),
      message: m.message({}, options),
      defaultCharacter: m.default_character({}, options),
      characterTyping: (characterName: string) => m.character_typing({ characterName }, options),
      generateResponseTitle: m.generate_response_title({}, options),
      generateResponse: m.generate_response({}, options),
      sendMessage: m.send_message({}, options),
      closedText: {
        '': '',
        host_left: m.closed_host_left({}, options),
        expired: m.closed_expired({}, options),
        not_found: m.closed_not_found({}, options),
        idle: m.closed_idle({}, options),
        slow: m.closed_slow({}, options),
        error: m.closed_error({}, options),
        left: m.closed_left({}, options),
      } satisfies Record<ClosedReason, string>,
      errorText: {
        '': '',
        invalid_link: m.error_invalid_link({}, options),
        missing_key: m.error_missing_key({}, options),
      } satisfies Record<JoinError, string>,
      systemParticipantJoined: (count: number) => m.system_participant_joined({ count }, options),
      systemParticipantLeft: (count: number) => m.system_participant_left({ count }, options),
      humanTyping: (name: string) => m.human_typing({ name }, options),
    };
  });

  const timeFormatter = $derived(
    new Intl.DateTimeFormat(localeState.current, { hour: '2-digit', minute: '2-digit' }),
  );

  $effect(() => {
    mpState.messages.length;
    const lastMessage = mpState.messages.at(-1);
    if (lastMessage?.kind === 'system') lastMessage.count;
    else lastMessage?.text;
    tick().then(() => logEl?.scrollTo({ top: logEl.scrollHeight }));
  });

  function formatTime(timestamp: number): string {
    if (!timestamp) return '';
    return timeFormatter.format(timestamp);
  }

  function formatSystemMessage(message: MpSystemMessage): string {
    return message.event === 'participant_joined'
      ? copy.systemParticipantJoined(message.count)
      : copy.systemParticipantLeft(message.count);
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
    <div class="gate-language"><LanguageSwitcher /></div>
    <section class="gate-card" aria-labelledby="join-title">
      <div class="brand-mark" aria-hidden="true">R</div>
      <p class="eyebrow">Ryokan Multiplayer</p>
      <h1 id="join-title">{copy.joinTitle}</h1>
      <p class="lead">{copy.joinIntro}</p>
      <form class="gate-form" onsubmit={(event) => { event.preventDefault(); submitLink(linkInput); }}>
        <label for="invite-link">{copy.invitationLink}</label>
        <input id="invite-link" type="url" bind:value={linkInput} placeholder={copy.invitationLinkPlaceholder} autocomplete="off" spellcheck="false" />
        <button class="primary-button" type="submit" disabled={!linkInput.trim()}>
          <span>{copy.continueButton}</span>
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.3" stroke-linecap="round" aria-hidden="true"><path d="M5 12h14M13 6l6 6-6 6" /></svg>
        </button>
      </form>
      {#if mpState.error}<p class="error" role="alert">{copy.errorText[mpState.error]}</p>{/if}
      <p class="privacy-note">
        <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><rect x="4" y="10" width="16" height="11" rx="2"/><path d="M8 10V7a4 4 0 0 1 8 0v3"/></svg>
        {copy.encryptedBeforeLeaving}
      </p>
    </section>
  </main>
{:else if mpState.view === 'name'}
  <main class="gate">
    <div class="gate-language"><LanguageSwitcher /></div>
    <section class="gate-card compact" aria-labelledby="name-title">
      <div class="room-badge" aria-hidden="true">
        <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2"/><circle cx="9" cy="7" r="4"/><path d="M22 21v-2a4 4 0 0 0-3-3.87M16 3.13a4 4 0 0 1 0 7.75"/></svg>
      </div>
      <p class="eyebrow">{copy.roomLabel(mpState.roomId || '·')}</p>
      <h1 id="name-title">{copy.nameTitle}</h1>
      <p class="lead">{copy.nameIntro}</p>
      <form class="gate-form" onsubmit={(event) => { event.preventDefault(); if (nameInput.trim()) enterRoom(nameInput); }}>
        <label for="display-name">{copy.yourName}</label>
        <input id="display-name" type="text" bind:value={nameInput} placeholder={copy.namePlaceholder} maxlength="32" autocomplete="nickname" />
        <button class="primary-button" type="submit" disabled={!nameInput.trim() || mpState.connecting}>{mpState.connecting ? copy.connecting : copy.joinRoom}</button>
      </form>
      {#if closed}<p class="error" role="alert">{copy.closedText[mpState.closedReason]}</p>{/if}
      <button class="text-button" type="button" onclick={backToJoin}>
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.3" stroke-linecap="round" aria-hidden="true"><path d="M19 12H5M11 18l-6-6 6-6"/></svg>
        {copy.back}
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
            <span class="participant-line"><i aria-hidden="true"></i>{copy.participantsInRoom(mpState.count)}</span>
          </div>
        </div>
        <div class="header-actions">
          <LanguageSwitcher />
          <span class="encrypted-label">
            <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><rect x="4" y="10" width="16" height="11" rx="2"/><path d="M8 10V7a4 4 0 0 1 8 0v3"/></svg>
            <span>{copy.endToEndEncrypted}</span>
          </span>
          <button class="leave-button" type="button" onclick={leave} aria-label={copy.leaveRoom}>
            <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4"/><path d="m16 17 5-5-5-5M21 12H9"/></svg>
            <span>{copy.leave}</span>
          </button>
        </div>
      </div>
    </header>

    <section class="transcript" bind:this={logEl} aria-live="polite">
      <div class="messages">
        {#each mpState.messages as message (message.id)}
          {#if message.kind === 'system'}
            <p class="system-message">{formatSystemMessage(message)}</p>
          {:else if message.kind === 'llm'}
            <article class="message ai-message">
              <span class="message-avatar {mpState.sessionCharacter?.color ?? ''}" aria-hidden="true">
                {#if mpState.sessionCharacter?.avatarUrl}<img src={mpState.sessionCharacter.avatarUrl} alt="" />{:else}{mpState.sessionCharacter?.initials ?? message.author[0]?.toUpperCase() ?? 'A'}{/if}
              </span>
              <div class="message-body">
                <div class="message-meta ai-meta"><strong>{message.author}</strong><time datetime={new Date(message.ts).toISOString()}>{formatTime(message.ts)}</time></div>
                <p>{message.text}{#if message.streaming}<span class="stream-dots" aria-label={copy.responseIsBeingGenerated}><i></i><i></i><i></i></span>{/if}</p>
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
        {#if mpState.remoteTypingName}
          <p class="human-typing" aria-live="polite">{copy.humanTyping(mpState.remoteTypingName)}</p>
        {/if}
        {#if mpState.messages.length === 0 && !mpState.remoteTypingName}
          <div class="empty-state">
            <span aria-hidden="true"><svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8"><path d="M21 15a4 4 0 0 1-4 4H8l-5 3V7a4 4 0 0 1 4-4h10a4 4 0 0 1 4 4z"/></svg></span>
            <p>{copy.emptyTitle}</p>
            <small>{copy.emptyDescription}</small>
          </div>
        {/if}
      </div>
    </section>

    {#if closed}
      <div class="closed-bar" role="alert"><span>{copy.closedText[mpState.closedReason]}</span><button class="text-button" type="button" onclick={backToJoin}>{copy.enterNewLink}</button></div>
    {:else}
      <form class="composer-shell" onsubmit={(event) => { event.preventDefault(); void submitChat(); }}>
        <div class="composer-wrap">
          <div class="composer">
            <textarea bind:this={composerEl} bind:value={chatInput} placeholder={locked ? copy.waitBriefly : copy.messagePlaceholder} disabled={locked} maxlength="4000" rows="1" aria-label={copy.message} oninput={() => { resizeComposer(); sendTyping(Boolean(chatInput.trim())); }} onkeydown={handleComposerKeydown}></textarea>
            <div class="composer-toolbar">
              <span class:active-lock={locked} class="room-status">
                {#if locked}<span class="typing-dot" aria-hidden="true"></span>{copy.characterTyping(mpState.characterName || copy.defaultCharacter)}{:else}{copy.participantsInRoom(mpState.count)}{/if}
              </span>
              <div class="composer-actions">
                {#if mpState.everyoneCanGenerate}
                  <button class="generate-button" type="button" onclick={requestGeneration} disabled={locked} title={copy.generateResponseTitle}>
                    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M12 3l1.9 5.7L19.6 10l-5.7 1.9L12 17.6l-1.9-5.7L4.4 10l5.7-1.9z"/></svg><span>{copy.generateResponse}</span>
                  </button>
                {/if}
                <button class="send-button" type="submit" disabled={locked || !chatInput.trim()} aria-label={copy.sendMessage}><svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M12 19V5M5 12l7-7 7 7"/></svg></button>
              </div>
            </div>
          </div>
        </div>
      </form>
    {/if}
  </main>
{/if}
