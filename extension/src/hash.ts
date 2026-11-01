// A checksum the page shows next to a download link: in the same line, the same table row, an
// element beside the link, or a `SHA256SUMS`-style list naming the file. Only the algorithm and
// the hex go to the app, never the page's text.

export type Algorithm = 'md5' | 'sha1' | 'sha256' | 'sha512';

export interface PageHash {
	algorithm: Algorithm;
	hex: string;
	/** Named on the page (`SHA-256: …`), not only guessed from its length. */
	labelled: boolean;
}

const BY_LENGTH: Record<number, Algorithm> = { 32: 'md5', 40: 'sha1', 64: 'sha256', 128: 'sha512' };
const STRENGTH: Algorithm[] = ['md5', 'sha1', 'sha256', 'sha512'];
const HEX = /(?<![0-9a-z_])(?:[0-9a-f]{128}|[0-9a-f]{64}|[0-9a-f]{40}|[0-9a-f]{32})(?![0-9a-z_])/gi;
/** A label just before a hash, e.g. `SHA-256:`, `sha256sum`, `MD5 checksum`. */
const LABEL = /(sha[-_ ]?512|sha[-_ ]?256|sha[-_ ]?1(?!\d)|md5)[^0-9a-z]{0,3}(?:sum|hash|checksum|digest)?[^0-9a-z]{0,4}$/i;

function labelAlgorithm(label: string): Algorithm {
	const l = label.toLowerCase().replace(/[-_ ]/g, '');
	return l === 'sha512' ? 'sha512' : l === 'sha256' ? 'sha256' : l === 'sha1' ? 'sha1' : 'md5';
}

/**
 * Every checksum in `text`. A hash with a label of its algorithm counts at any length; one
 * without counts only at 64 or 128 digits (SHA-256, SHA-512): 40 and 32 unlabelled digits are
 * as often a commit or an id as a SHA-1 or an MD5.
 */
export function hashesIn(text: string): PageHash[] {
	const out: PageHash[] = [];
	for (const m of text.matchAll(HEX)) {
		const hex = m[0];
		// All digits is a number; all letters is a word.
		if (!/[0-9]/.test(hex) || !/[a-f]/i.test(hex)) continue;
		const before = text.slice(Math.max(0, (m.index ?? 0) - 24), m.index);
		const label = before.match(LABEL);
		const algorithm = BY_LENGTH[hex.length];
		if (label) {
			if (labelAlgorithm(label[1]) !== algorithm) continue; // a label for something else
			out.push({ algorithm, hex: hex.toLowerCase(), labelled: true });
		} else if (hex.length >= 64) {
			out.push({ algorithm, hex: hex.toLowerCase(), labelled: false });
		}
	}
	return out;
}

/**
 * The one checksum to check against, or `null` when there is none or two that disagree: the
 * strongest algorithm found, labelled ones before guessed ones.
 */
export function pick(found: PageHash[]): PageHash | null {
	if (found.length === 0) return null;
	const rank = (h: PageHash) => STRENGTH.indexOf(h.algorithm) + (h.labelled ? 10 : 0);
	const best = Math.max(...found.map(rank));
	const top = new Set(found.filter((h) => rank(h) === best).map((h) => h.hex));
	if (top.size !== 1) return null;
	return found.find((h) => rank(h) === best) ?? null;
}

/** A checksum on a line of `text` that names the file `name` (`<hex>  name`, `name: <hex>`). */
export function hashForName(text: string, name: string): PageHash | null {
	if (!name) return null;
	// The whole name, not a part of a longer one (`a.zip` in `a.zip.asc` or `data.zip`).
	const lines = text.split(/\r?\n/).filter((l) => {
		for (let i = l.indexOf(name); i >= 0; i = l.indexOf(name, i + 1)) {
			if (!/[\w.-]/.test(l[i - 1] ?? '') && !/[\w.-]/.test(l[i + name.length] ?? '')) return true;
		}
		return false;
	});
	const found = lines.flatMap((l) => {
		// A checksum list's line: the name and the hash, perhaps a label, nothing else
		// (`<hex>  name`, `<hex> *name`, `SHA256 (name) = <hex>`, `name: <hex>`).
		const rest = l.replace(name, '').replace(HEX, '');
		if (!/^[\s*:=()|,-]*(?:(?:sha[-_ ]?\d+|md5)[\s*:=()|,-]*)?$/i.test(rest)) return [];
		const own = hashesIn(l);
		// In a checksum list the length says the algorithm: `sha1sum` output has no label.
		if (own.length > 0) return own;
		return [...l.matchAll(HEX)]
			.map((m) => m[0])
			.filter((hex) => /[0-9]/.test(hex) && /[a-f]/i.test(hex))
			.map((hex) => ({ algorithm: BY_LENGTH[hex.length], hex: hex.toLowerCase(), labelled: false }));
	});
	return pick(found);
}

/** `algorithm:hex`, as the app takes checksums. */
export const asChecksum = (h: PageHash) => `${h.algorithm}:${h.hex}`;

// ---- In the page ----------------------------------------------------------------------------

/** Files a checksum or a signature is kept in: links to them sit next to the file's own. */
const COMPANION = /\.(asc|sig|sign|gpg|minisig|sha\d*|md5|sha\d*sum|txt|json|torrent|meta4|sbom|intoto\.jsonl)$/i;

function fileOf(href: string): string {
	try {
		const path = new URL(href).pathname;
		return decodeURIComponent(path.slice(path.lastIndexOf('/') + 1));
	} catch {
		return '';
	}
}

/** Another file's download link in `scope`: the hash in it may be that file's. */
function hasOtherFile(scope: Element, link: HTMLAnchorElement): boolean {
	for (const a of scope.querySelectorAll<HTMLAnchorElement>('a[href]')) {
		if (a === link || a.href === link.href) continue;
		const name = fileOf(a.href);
		if (/\.[a-z0-9]{1,5}$/i.test(name) && !COMPANION.test(name)) return true;
	}
	return false;
}

/** Visible text, with rows and blocks on lines of their own. */
const textOf = (el: Element) => (el as HTMLElement).innerText ?? el.textContent ?? '';

/**
 * The checksum shown for the file `link` downloads: in the smallest part of the page around
 * the link that holds no other file's link (the link's line, its table row, its list item),
 * in the elements right after it, or on a line of the page that names the file.
 */
export function hashNear(link: HTMLAnchorElement): PageHash | null {
	let scope: Element | null = link;
	for (let depth = 0; scope && depth < 6; depth++) {
		if (depth > 0 && hasOtherFile(scope, link)) break;
		const own = pick(hashesIn(textOf(scope)));
		if (own) return own;
		// `<a>…</a>` then `<code>…</code>` or `<p>SHA-256: …</p>` below it.
		let sibling = scope.nextElementSibling;
		for (let i = 0; sibling && i < 2; i++, sibling = sibling.nextElementSibling) {
			if (sibling.querySelector('a[href]') && hasOtherFile(sibling, link)) break;
			const next = pick(hashesIn(textOf(sibling)));
			if (next) return next;
		}
		scope = scope.parentElement;
	}
	return hashForName(textOf(link.ownerDocument.body), fileOf(link.href));
}
