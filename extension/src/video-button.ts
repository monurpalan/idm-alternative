// A "Download" button by videos, like IDM's: it appears just outside the top right corner of
// the video the pointer is over (see place.ts), so it never covers the picture. Clicking it
// lists the qualities the video offers right there on the page (at once on YouTube and TikTok,
// from what the site gave its player: page-data.ts; else as the app reads them with yt-dlp);
// the one picked goes to the app, which opens on it to confirm the name and folder. A plain
// file, with nothing to pick, goes at once. Drawn in a closed shadow root so the page's styles
// cannot reach it.

import { videoAddress, type Hints } from './address.ts';
import { asNoted, qualitiesOf, videoKey, type Formats, type Quality } from './formats.ts';
import { placeButton } from './place.ts';

/** Videos smaller than this are previews, avatars or ads. */
const MIN_WIDTH = 200;
const MIN_HEIGHT = 120;
const LABEL = '⬇ Download';

let host: HTMLElement | null = null;
let button: HTMLButtonElement | null = null;
let menu: HTMLElement | null = null;
let current: HTMLVideoElement | null = null;
let hideTimer: number | undefined;
/** The video the open menu is for; the button stays while it is open. */
let menuFor: HTMLVideoElement | null = null;

function build() {
	host = document.createElement('shardload-video-button');
	host.style.cssText = 'all: initial; position: fixed; z-index: 2147483647; display: none;';
	const root = host.attachShadow({ mode: 'closed' });
	root.innerHTML = `
		<style>
			/* The app's colors (see ui.css); the page's styles cannot reach a shadow root, so
			   they are spelled out here. */
			/* On the button and the menu, not :host: the host's inline \`all: initial\` wins over it. */
			.main, .menu { font: 600 12px system-ui, 'Segoe UI', sans-serif; }
			button {
				font: inherit;
				border: 0;
				cursor: pointer;
			}
			.main {
				display: block;
				min-height: 24px;
				padding: 0 10px;
				border-radius: 4px;
				background: #2563eb;
				color: #f3f1ec;
				box-shadow: 0 2px 10px color-mix(in srgb, #16181d 35%, transparent);
				opacity: 0.92;
				white-space: nowrap;
			}
			.main:hover { opacity: 1; }
			.main[disabled] { cursor: default; opacity: 1; }
			.menu {
				position: absolute;
				top: calc(100% + 2px);
				right: 0;
				min-width: 160px;
				max-height: 320px;
				overflow-y: auto;
				padding: 4px;
				border-radius: 6px;
				background: #1c1f26;
				color: #f3f1ec;
				box-shadow: 0 6px 24px color-mix(in srgb, #000 45%, transparent);
			}
			.menu[hidden] { display: none; }
			.menu button {
				display: block;
				width: 100%;
				padding: 6px 10px;
				border-radius: 4px;
				background: transparent;
				color: inherit;
				text-align: left;
				white-space: nowrap;
			}
			.menu button:hover, .menu button:focus-visible { background: #2563eb; outline: none; }
			.note { padding: 6px 10px; color: #b9b6ae; font-weight: 400; white-space: nowrap; }
		</style>
		<button type="button" class="main" title="Download with Shardload">${LABEL}</button>
		<div class="menu" role="menu" hidden></div>`;
	button = root.querySelector('.main');
	menu = root.querySelector('.menu');
	button?.addEventListener('click', (e) => {
		e.preventDefault();
		e.stopPropagation();
		if (menuFor) closeMenu();
		else if (current) openMenu(current);
	});
	// Clicks inside stay inside: the page must not take them as clicks on the player.
	for (const type of ['pointerdown', 'mousedown', 'mouseup', 'click', 'dblclick']) {
		host.addEventListener(type, (e) => e.stopPropagation());
	}
	host.addEventListener('pointerenter', () => clearTimeout(hideTimer));
	host.addEventListener('pointerleave', scheduleHide);
	document.documentElement.append(host);
}

function place(video: HTMLVideoElement) {
	if (!host) build();
	if (!host) return;
	host.style.display = 'block';
	const r = video.getBoundingClientRect();
	const b = button?.getBoundingClientRect();
	const p = placeButton(
		r,
		{ width: b?.width || 100, height: b?.height || 24 },
		{ width: document.documentElement.clientWidth, height: innerHeight }
	);
	host.style.top = `${p.top}px`;
	host.style.left = `${p.left}px`;
	if (current !== video) readAheadOf(video);
	current = video;
}

interface QualitiesReply {
	ok: boolean;
	error?: string;
	qualities?: Quality[];
	/** What they were read for, given back with the choice. */
	items?: unknown[];
}

/** How long the pointer rests on a video before its qualities are read ahead of a click. */
const READ_AHEAD_MS = 600;
let aheadTimer: number | undefined;
/** Qualities read ahead, for the video (and its source) they were read for. */
let ahead: { video: HTMLVideoElement; src: string; reply: Promise<QualitiesReply> } | null = null;

function askQualities(video: HTMLVideoElement, readAhead: boolean): Promise<QualitiesReply> {
	return chrome.runtime
		.sendMessage({ videoQualities: { src: srcOf(video), hints: hintsFor(video), ahead: readAhead } })
		.catch((err: unknown) => ({ ok: false, error: String(err) })) as Promise<QualitiesReply>;
}

/**
 * Reading the qualities takes yt-dlp a few seconds; a pointer resting on a video starts it,
 * so the list is (nearly) there when the button is pressed. Only an app already running
 * answers: resting on a video never starts it.
 */
function readAheadOf(video: HTMLVideoElement) {
	clearTimeout(aheadTimer);
	aheadTimer = window.setTimeout(() => {
		const src = srcOf(video);
		if (current !== video || menuFor || (ahead?.video === video && ahead.src === src)) return;
		ahead = { video, src, reply: askQualities(video, true) };
	}, READ_AHEAD_MS);
}

function scheduleHide() {
	clearTimeout(hideTimer);
	if (menuFor) return; // stays while its menu is open
	hideTimer = window.setTimeout(() => {
		if (host) host.style.display = 'none';
		current = null;
	}, 1200);
}

function note(text: string) {
	if (!menu) return;
	const p = document.createElement('div');
	p.className = 'note';
	p.textContent = text;
	menu.replaceChildren(p);
}

function srcOf(video: HTMLVideoElement): string {
	return video.currentSrc || video.src;
}

/** Links and ids around the video, closest first, for finding its own address (address.ts). */
function hintsFor(video: HTMLVideoElement): Hints {
	const around: string[] = [];
	const near: string[] = [];
	const ids: string[] = [];
	const post: string[] = [];
	let timed = false;
	const r = video.getBoundingClientRect();
	const area = Math.max(1, r.width * r.height);
	let e: Element | null = video;
	for (let depth = 0; e && depth < 16; depth++, e = e.parentElement) {
		if (e.id) ids.push(e.id);
		if (e instanceof HTMLAnchorElement && e.href) around.push(e.href);
		// The post the video is in: a `permalink` (Reddit), or, in the closest box that has
		// one, the links holding a time (how nearly every social site links a post).
		const permalink = e.getAttribute('permalink');
		if (permalink) post.push(new URL(permalink, location.href).href);
		if (!timed) {
			const links = Array.from(e.querySelectorAll('a[href]')).filter((a) => a.querySelector('time'));
			if (links.length) {
				timed = true;
				for (const a of links.slice(0, 5)) post.push((a as HTMLAnchorElement).href);
			}
		}
		// Links inside boxes not much larger than the video: its card, not the whole feed.
		const b = e.getBoundingClientRect();
		if (near.length < 50 && b.width * b.height <= area * 4) {
			for (const a of Array.from(e.querySelectorAll('a[href]')).slice(0, 20)) near.push((a as HTMLAnchorElement).href);
		}
	}
	return { page: location.href, around, near, ids, post };
}

/** How long the page's own data (page-data.ts) is waited for; it answers in a moment or not. */
const PAGE_WAIT_MS = 150;

/**
 * The formats the site gave its player for this video, noted in the page's world (page-data.ts,
 * on YouTube and TikTok), or `null` when there are none: the app reads them then.
 */
function pageFormats(video: HTMLVideoElement): Promise<Formats | null> {
	const key = videoKey(videoAddress(hintsFor(video)).url);
	if (!key) return Promise.resolve(null);
	return new Promise((resolve) => {
		const done = (f: Formats | null) => {
			removeEventListener('message', heard);
			clearTimeout(timer);
			resolve(f && f.heights.length ? f : null);
		};
		const heard = (e: MessageEvent) => {
			if (e.source !== window || e.data?.type !== 'shardload:formats' || e.data.key !== key) return;
			done(asNoted({ key, formats: e.data.formats })?.formats ?? null);
		};
		const timer = window.setTimeout(() => done(null), PAGE_WAIT_MS);
		addEventListener('message', heard);
		window.postMessage({ type: 'shardload:formats?', key }, location.origin);
	});
}

async function openMenu(video: HTMLVideoElement) {
	if (!menu || !button) return;
	menuFor = video;
	clearTimeout(hideTimer);
	note('Reading the qualities…');
	menu.hidden = false;
	button.disabled = true;
	clearTimeout(aheadTimer);
	// At once, from what the site gave the page.
	const fromPage = await pageFormats(video);
	if (menuFor !== video) return;
	if (fromPage) {
		button.disabled = false;
		list(video, qualitiesOf(fromPage));
		return;
	}
	const early = ahead?.video === video && ahead.src === srcOf(video) ? await ahead.reply : null;
	// Not read ahead, or the app was not running then.
	const reply = early?.ok ? early : await askQualities(video, false);
	if (menuFor !== video) return; // closed meanwhile
	button.disabled = false;
	if (!reply?.ok) {
		note(reply?.error ?? 'Not available');
		return;
	}
	const qualities = reply.qualities ?? [];
	if (!qualities.length) {
		// Nothing to choose (a plain file), or the app reads it itself.
		closeMenu();
		send(video, null);
		return;
	}
	list(video, qualities, reply.items);
}

/** Lists the qualities; the one picked goes with the items they were read for, if any. */
function list(video: HTMLVideoElement, qualities: Quality[], items?: unknown[]) {
	if (!menu) return;
	menu.replaceChildren(
		...qualities.map((q) => {
			const item = document.createElement('button');
			item.type = 'button';
			item.setAttribute('role', 'menuitem');
			item.textContent = q.label;
			item.addEventListener('click', (e) => {
				e.preventDefault();
				closeMenu();
				send(video, q.id, items);
			});
			return item;
		})
	);
	(menu.firstElementChild as HTMLElement | null)?.focus();
}

function closeMenu() {
	menuFor = null;
	if (menu) {
		menu.hidden = true;
		menu.replaceChildren();
	}
	if (button) button.disabled = false;
}

/** Hands the video to the app, with the quality picked (`null`: the app asks). */
async function send(video: HTMLVideoElement, choice: string | null, items?: unknown[]) {
	if (!button) return;
	button.disabled = true;
	button.textContent = 'Sending…';
	const reply = (await chrome.runtime
		.sendMessage({ videoButton: { src: srcOf(video), hints: hintsFor(video), choice, items } })
		.catch((err: unknown) => ({ ok: false, error: String(err) }))) as { ok: boolean; error?: string };
	button.textContent = reply?.ok ? 'Sent ✓' : 'Not available';
	button.title = reply?.ok ? 'Sent to Shardload' : (reply?.error ?? '');
	setTimeout(() => {
		if (!button) return;
		button.disabled = false;
		button.textContent = LABEL;
		button.title = 'Download with Shardload';
	}, 2000);
}

/** The large video under the pointer, also when an overlay (controls, a div) covers it. */
function videoAt(x: number, y: number): HTMLVideoElement | null {
	for (const v of Array.from(document.querySelectorAll('video'))) {
		const r = v.getBoundingClientRect();
		if (r.width < MIN_WIDTH || r.height < MIN_HEIGHT) continue;
		if (x >= r.left && x <= r.right && y >= r.top && y <= r.bottom) return v;
	}
	return null;
}

let pending = false;
addEventListener(
	'pointermove',
	(e) => {
		// At most one check per frame: pointer events come fast.
		if (pending || menuFor) return;
		pending = true;
		requestAnimationFrame(() => {
			pending = false;
			const v = videoAt(e.clientX, e.clientY);
			if (v) {
				clearTimeout(hideTimer);
				place(v);
			} else if (current) {
				scheduleHide();
			}
		});
	},
	{ passive: true, capture: true }
);

// A click elsewhere or Escape closes the menu.
addEventListener(
	'pointerdown',
	(e) => {
		if (menuFor && e.target !== host) {
			closeMenu();
			scheduleHide();
		}
	},
	{ capture: true }
);
addEventListener(
	'keydown',
	(e) => {
		if (menuFor && e.key === 'Escape') {
			closeMenu();
			scheduleHide();
		}
	},
	{ capture: true }
);

// Keep the button on its video while the page scrolls.
addEventListener(
	'scroll',
	() => {
		if (current && host?.style.display === 'block') place(current);
	},
	{ passive: true, capture: true }
);
