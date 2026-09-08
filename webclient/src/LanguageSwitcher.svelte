<script lang="ts">
  import { changeLocale, localeState } from './i18n.svelte';
  import * as m from './paraglide/messages.js';

  const copy = $derived.by(() => {
    const locale = localeState.current;
    const options = { locale };
    return {
      label: m.language_switcher_label({}, options),
      english: m.switch_to_english({}, options),
      german: m.switch_to_german({}, options),
    };
  });
</script>

<div class="language-switcher" role="group" aria-label={copy.label}>
  <button
    type="button"
    class:active={localeState.current === 'en'}
    aria-pressed={localeState.current === 'en'}
    aria-label={copy.english}
    title={copy.english}
    onclick={() => void changeLocale('en')}
  >EN</button>
  <button
    type="button"
    class:active={localeState.current === 'de'}
    aria-pressed={localeState.current === 'de'}
    aria-label={copy.german}
    title={copy.german}
    onclick={() => void changeLocale('de')}
  >DE</button>
</div>
