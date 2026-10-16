// How numbers look in the list: binary units like the CLI, short durations.

const UNITS = ['B', 'KB', 'MB', 'GB', 'TB'];

export function bytes(n: number | null | undefined): string {
	if (n === null || n === undefined) return '';
	let v = n;
	let i = 0;
	while (v >= 1024 && i < UNITS.length - 1) {
		v /= 1024;
		i++;
	}
	return i === 0 ? `${v} B` : `${v.toFixed(v < 10 ? 2 : 1)} ${UNITS[i]}`;
}

export function speed(bps: number | null | undefined): string {
	return bps ? `${bytes(Math.round(bps))}/s` : '';
}

export function duration(secs: number | null | undefined): string {
	if (secs === null || secs === undefined || !isFinite(secs)) return '';
	const s = Math.max(0, Math.round(secs));
	if (s < 60) return `${s} s`;
	const m = Math.floor(s / 60);
	if (m < 60) return `${m} min ${s % 60} s`;
	const h = Math.floor(m / 60);
	return h < 48 ? `${h} h ${m % 60} min` : `${Math.floor(h / 24)} days`;
}

export function date(ms: number): string {
	const d = new Date(ms);
	const today = new Date();
	return d.toDateString() === today.toDateString()
		? d.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' })
		: d.toLocaleDateString();
}
