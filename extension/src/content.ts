// Tells the background when Alt is held: an Alt+click download stays in the browser, like
// IDM's bypass key.

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
