// How numbers look in the list: binary units like the CLI, short durations.

import { locale, t } from './i18n.svelte.ts';

const UNITS = ['B', 'KB', 'MB', 'GB', 'TB'];

export function bytes(n: number | null | undefined): string {
	if (n === null || n === undefined) return '';
	let v = n;
	let i = 0;
	while (v >= 1024 && i < UNITS.length - 1) {
		v /= 1024;
		i++;
	}
	const digits = v < 10 ? 2 : 1;
	const shown = v.toLocaleString(locale(), {
		minimumFractionDigits: digits,
		maximumFractionDigits: digits
	});
	return i === 0 ? `${v} B` : `${shown} ${UNITS[i]}`;
}

export function speed(bps: number | null | undefined): string {
	return bps ? t('{size}/s', { size: bytes(Math.round(bps)) }) : '';
}

export function duration(secs: number | null | undefined): string {
	if (secs === null || secs === undefined || !isFinite(secs)) return '';
	const s = Math.max(0, Math.round(secs));
	if (s < 60) return t('{s} s', { s });
	const m = Math.floor(s / 60);
	if (m < 60) return t('{m} min {s} s', { m, s: s % 60 });
	const h = Math.floor(m / 60);
	return h < 48 ? t('{h} h {m} min', { h, m: m % 60 }) : t('{d} days', { d: Math.floor(h / 24) });
}

/** A fraction (0..1) as a percentage with one decimal: "29.2%", Turkish "%29,2". */
export function percent(fraction: number): string {
	const p = (fraction * 100).toLocaleString(locale(), {
		minimumFractionDigits: 1,
		maximumFractionDigits: 1
	});
	return t('{p}%', { p });
}

export function date(ms: number): string {
	const d = new Date(ms);
	const today = new Date();
	return d.toDateString() === today.toDateString()
		? d.toLocaleTimeString(locale(), { hour: '2-digit', minute: '2-digit' })
		: d.toLocaleDateString(locale());
}
