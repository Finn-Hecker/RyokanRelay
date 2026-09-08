import * as m from './paraglide/messages.js';
import {
  getLocale,
  getTextDirection,
  setLocale,
  type Locale,
} from './paraglide/runtime.js';

export const localeState = $state({ current: getLocale() });

function syncDocument(locale: Locale): void {
  document.documentElement.lang = locale;
  document.documentElement.dir = getTextDirection(locale);
  document.title = m.document_title({}, { locale });
}

export function initializeLocale(): void {
  syncDocument(localeState.current);
}

export async function changeLocale(locale: Locale): Promise<void> {
  if (locale === localeState.current) return;
  await setLocale(locale, { reload: false });
  localeState.current = locale;
  syncDocument(locale);
}
