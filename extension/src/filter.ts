// Which downloads the extension hands to the app. Pure functions, tested with node:test.

export interface Settings {
	/** Hand downloads over at all. */
	enabled: boolean;
	/** Leave smaller downloads to the browser, in KB; 0 for no minimum. */
	minSizeKb: number;
	/** Only these extensions (lowercase, no dot); empty for every file. */
	extensions: string[];
	/** Never take downloads from these sites (and their subdomains). */
	excludedSites: string[];
}

/** IDM's default list of file types to take over. */
export const DEFAULT_EXTENSIONS = [
	'3gp', '7z', 'aac', 'apk', 'arj', 'avi', 'bin', 'bz2', 'cab', 'deb', 'dmg', 'exe', 'flac',
	'flv', 'gz', 'img', 'iso', 'm4a', 'm4v', 'mkv', 'mov', 'mp3', 'mp4', 'mpeg', 'mpg', 'msi',
	'msix', 'ogg', 'ogv', 'pdf', 'pkg', 'rar', 'rpm', 'tar', 'tgz', 'wav', 'webm', 'wma', 'wmv',
	'xz', 'zip', 'zst'
];

export const DEFAULT_SETTINGS: Settings = {
	enabled: true,
	minSizeKb: 0,
	extensions: DEFAULT_EXTENSIONS,
	excludedSites: []
};

/** What the browser tells about a download as it starts (the parts we use). */
export interface Candidate {
	url: string;
	/** Name or path the browser picked; often empty at this point. */
	filename: string;
	/** Bytes, or -1/0 when not known yet. */
	size: number;
	/** Started by this extension itself (its fallback after the app could not take it). */
	ours: boolean;
}

/** `true` when the app should take this download over. */
export function shouldCapture(c: Candidate, s: Settings, altHeld: boolean): boolean {
	if (!s.enabled || c.ours || altHeld) return false;
	let url: URL;
	try {
		url = new URL(c.url);
	} catch {
		return false;
	}
	// blob: and data: downloads exist only inside the page; the app cannot fetch them.
	if (url.protocol !== 'http:' && url.protocol !== 'https:') return false;
	if (isExcluded(url.hostname, s.excludedSites)) return false;
	if (s.minSizeKb > 0 && c.size > 0 && c.size < s.minSizeKb * 1024) return false;
	if (s.extensions.length) {
		const ext = extension(baseName(c.filename) || baseName(url.pathname));
		// Without an extension there is nothing to filter on: the app asks the server.
		if (ext && !s.extensions.includes(ext)) return false;
	}
	return true;
}

/** `host` is one of `sites`, or a subdomain of one. */
export function isExcluded(host: string, sites: string[]): boolean {
	const h = host.toLowerCase();
	return sites.some((site) => {
		const s = site.trim().toLowerCase().replace(/^\*?\./, '');
		return s !== '' && (h === s || h.endsWith(`.${s}`));
	});
}

/** The last part of a path, either separator, percent-decoded when possible. */
export function baseName(path: string): string {
	const last = path.split(/[\\/]/).filter(Boolean).pop() ?? '';
	try {
		return decodeURIComponent(last);
	} catch {
		return last;
	}
}

export function extension(name: string): string {
	const i = name.lastIndexOf('.');
	return i > 0 ? name.slice(i + 1).toLowerCase() : '';
}

/** A `Cookie` header from cookies the browser holds for the URL. */
export function cookieHeader(cookies: { name: string; value: string }[]): string | null {
	return cookies.length ? cookies.map((c) => `${c.name}=${c.value}`).join('; ') : null;
}

/** "zip, .pdf  rar" → ["zip", "pdf", "rar"]; "example.com\nfoo.org" → sites. */
export function parseList(text: string): string[] {
	return [
		...new Set(
			text
				.split(/[\s,;]+/)
				.map((w) => w.trim().toLowerCase().replace(/^\./, ''))
				.filter(Boolean)
		)
	];
}
