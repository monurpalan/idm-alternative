// Lists English texts passed to t('…') that have no Turkish text, keys that appear in two
// Turkish files, and Turkish texts nothing uses. Exits 1 on the first two.
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const src = fileURLToPath(new URL('../src', import.meta.url));
const { parts } = await import(new URL('../src/lib/i18n/tr/index.ts', import.meta.url).href);

function* files(dir) {
	for (const name of readdirSync(dir)) {
		const p = join(dir, name);
		if (statSync(p).isDirectory()) {
			if (name !== 'i18n') yield* files(p);
		} else if (/\.(svelte|ts)$/.test(name) && name !== 'i18n.svelte.ts') yield p;
	}
}

// t('text'), msg('text')…: the first argument when it is a literal (not a `…${x}…` one).
const call = /\b(?:t|msg)\(\s*(?:'((?:\\.|[^'\\])*)'|"((?:\\.|[^"\\])*)"|`((?:\\.|[^`\\$])*)`)/g;
const unescape = (s) => s.replace(/\\(.)/g, '$1');
const used = new Map();
for (const f of files(src)) {
	const text = readFileSync(f, 'utf8');
	for (const m of text.matchAll(call)) {
		const key = unescape(m[1] ?? m[2] ?? m[3]);
		if (!used.has(key)) used.set(key, f.slice(src.length + 1));
	}
}

const owner = new Map();
let failed = false;
for (const [part, dict] of Object.entries(parts)) {
	for (const key of Object.keys(dict)) {
		if (owner.has(key)) {
			console.log(`in both ${owner.get(key)} and ${part}: ${key}`);
			failed = true;
		}
		owner.set(key, part);
	}
}
for (const [key, file] of used) {
	if (!owner.has(key)) {
		console.log(`no Turkish text (${file}): ${key}`);
		failed = true;
	}
}
const unused = [...owner.keys()].filter((k) => !used.has(k));
if (unused.length) console.log(`unused Turkish texts:\n  ${unused.join('\n  ')}`);
console.log(`${used.size} texts, ${owner.size} translated`);
process.exit(failed ? 1 : 0);
