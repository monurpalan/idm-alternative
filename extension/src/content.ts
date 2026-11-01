// Tells the background when Alt is held: an Alt+click download stays in the browser, like
// IDM's bypass key. Remembers the link last clicked, so the background can ask for a checksum
// the page shows next to it (see `hash.ts`).

import { asChecksum, hashNear } from './hash.ts';

addEventListener(
	'keydown',
	(e) => {
		if (e.key === 'Alt') chrome.runtime.sendMessage({ alt: true }).catch(() => {});
	},
	true
);

addEventListener(
	'mousedown',
	(e) => {
		if (e.altKey) chrome.runtime.sendMessage({ alt: true }).catch(() => {});
	},
	true
);

/** The link last clicked (or middle-clicked), and when. */
let clicked: { link: HTMLAnchorElement; at: number } | null = null;

for (const type of ['click', 'auxclick']) {
	addEventListener(
		type,
		(e) => {
			const link = (e.target as Element | null)?.closest?.('a[href]');
			if (link instanceof HTMLAnchorElement) clicked = { link, at: Date.now() };
		},
		true
	);
}

/** The link on this page that started a download of one of `urls`. */
function linkTo(urls: string[]): HTMLAnchorElement | null {
	if (clicked && (urls.includes(clicked.link.href) || Date.now() - clicked.at < 5000)) return clicked.link;
	return [...document.querySelectorAll<HTMLAnchorElement>('a[href]')].find((a) => urls.includes(a.href)) ?? null;
}

// `{ hashFor: [urls] }`: the checksum shown next to the link, as `algorithm:hex`. A frame
// without the link stays quiet, so the one with it answers.
chrome.runtime.onMessage.addListener(
	(message: { hashFor?: string[] }, _sender, reply: (checksum: string | null) => void) => {
		if (!Array.isArray(message?.hashFor)) return false;
		const link = linkTo(message.hashFor);
		if (!link) return false;
		const found = hashNear(link);
		reply(found ? asChecksum(found) : null);
		return false;
	}
);
