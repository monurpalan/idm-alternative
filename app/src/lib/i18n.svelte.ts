// The interface in English or Turkish. Texts stay English in the code, `t('Add download')`;
// the Turkish ones are in `i18n/tr/`, one file per part of the UI. `npm run i18n` lists any
// English text without a Turkish one (CI runs it).

import { tr } from './i18n/tr/index.ts';

export type Lang = 'en' | 'tr';

let lang = $state<Lang>('en');

export const i18n = {
	get lang(): Lang {
		return lang;
	},
	set(l: Lang) {
		lang = l;
		document.documentElement.lang = l;
	}
};

/** `text` in the current language; `{name}` in it is replaced from `params`. */
export function t(text: string, params?: Record<string, string | number>): string {
	const s = lang === 'tr' ? (tr[text] ?? text) : text;
	return params ? s.replace(/\{(\w+)\}/g, (m, k) => (k in params ? String(params[k]) : m)) : s;
}

/** Marks a text kept for later, shown with `t(text)` (`npm run i18n` finds it here). */
export const msg = (text: string): string => text;

/** The locale for numbers and dates. */
export const locale = (): string => (lang === 'tr' ? 'tr-TR' : 'en-GB');
