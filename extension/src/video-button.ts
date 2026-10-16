// A "Download" button over videos, like IDM's: it appears at the top right corner of a video
// the pointer is over, and hands that video (or the stream the page plays through it) to the
// app. Drawn in a closed shadow root so the page's styles cannot reach it.

/** Videos smaller than this are previews, avatars or ads. */
const MIN_WIDTH = 200;
const MIN_HEIGHT = 120;

let host: HTMLElement | null = null;
let button: HTMLButtonElement | null = null;
let current: HTMLVideoElement | null = null;
let hideTimer: number | undefined;

function build() {
	host = document.createElement('idma-video-button');
	host.style.cssText = 'all: initial; position: fixed; z-index: 2147483647; display: none;';
	const root = host.attachShadow({ mode: 'closed' });
	root.innerHTML = `
		<style>
			button {
				font: 600 12px system-ui, 'Segoe UI', sans-serif;
				color: #fff;
				background: #2563eb;
				border: 0;
				border-radius: 6px;
				padding: 6px 10px;
				cursor: pointer;
				box-shadow: 0 2px 8px rgb(0 0 0 / 0.35);
				opacity: 0.92;
			}
			button:hover { opacity: 1; }
			button[disabled] { background: #6b6b76; cursor: default; }
		</style>
		<button type="button" title="Download with idm-alternative">⬇ Download</button>`;
	button = root.querySelector('button');
	button?.addEventListener('click', async (e) => {
		e.preventDefault();
		e.stopPropagation();
		if (!current || !button) return;
		button.disabled = true;
		button.textContent = 'Sending…';
		const reply = (await chrome.runtime
			.sendMessage({ videoButton: { src: current.currentSrc || current.src } })
			.catch((err: unknown) => ({ ok: false, error: String(err) }))) as { ok: boolean; error?: string };
		button.textContent = reply?.ok ? 'Sent ✓' : 'Not available';
		button.title = reply?.ok ? 'Sent to idm-alternative' : (reply?.error ?? '');
		setTimeout(() => {
			if (!button) return;
			button.disabled = false;
			button.textContent = '⬇ Download';
			button.title = 'Download with idm-alternative';
		}, 2000);
	});
	host.addEventListener('pointerenter', () => clearTimeout(hideTimer));
	host.addEventListener('pointerleave', scheduleHide);
	document.documentElement.append(host);
}

function place(video: HTMLVideoElement) {
	if (!host) build();
	const r = video.getBoundingClientRect();
	if (!host) return;
	host.style.top = `${Math.max(4, r.top + 8)}px`;
	host.style.left = `${Math.max(4, r.right - 120)}px`;
	host.style.display = 'block';
	current = video;
}

function scheduleHide() {
	clearTimeout(hideTimer);
	hideTimer = window.setTimeout(() => {
		if (host) host.style.display = 'none';
		current = null;
	}, 1200);
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
		if (pending) return;
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

// Keep the button on its video while the page scrolls.
addEventListener(
	'scroll',
	() => {
		if (current && host?.style.display === 'block') place(current);
	},
	{ passive: true, capture: true }
);
