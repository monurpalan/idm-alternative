// Telling the app which version of the extension runs in which browser, so the app can say
// when none is installed or one is too old. Kept free of the extension API for the tests.

/**
 * The version of the messages between the extension and the app (`PROTOCOL` in
 * crates/bridge). Raised when a change means an older extension no longer works with the app.
 */
export const PROTOCOL = 1;

/** A hello the app answered counts this long. */
export const HELLO_EVERY_MS = 60 * 60 * 1000;
/** After one that failed (the app was not running), the next try waits this long. */
export const RETRY_MS = 5 * 60 * 1000;

export interface HelloTimes {
	/** When the app last answered. */
	ok?: number;
	/** When the last try was. */
	tried?: number;
}

export function helloDue(last: HelloTimes, now: number): boolean {
	if (last.ok !== undefined && now - last.ok < HELLO_EVERY_MS) return false;
	return last.tried === undefined || now - last.tried >= RETRY_MS;
}

export interface BrowserHints {
	/** Built for Firefox. */
	firefox: boolean;
	/** `navigator.userAgentData.brands`, where there is one. */
	brands: string[];
	/** Brave says so through `navigator.brave`. */
	brave: boolean;
	userAgent: string;
}

/** `chrome`, `edge`, `brave`, `opera`, `vivaldi`, `chromium` or `firefox`. */
export function browserName(h: BrowserHints): string {
	if (h.firefox) return 'firefox';
	if (h.brave) return 'brave';
	const brand = (name: string) => h.brands.includes(name);
	if (brand('Microsoft Edge') || /\bEdg\//.test(h.userAgent)) return 'edge';
	if (brand('Opera') || /\bOPR\//.test(h.userAgent)) return 'opera';
	if (/\bVivaldi\//.test(h.userAgent)) return 'vivaldi';
	if (brand('Google Chrome')) return 'chrome';
	return 'chromium';
}

export function helloMessage(version: string, browser: string) {
	return { type: 'extension', version, browser, protocol: PROTOCOL };
}

/**
 * Whether `sendNativeMessage` failed because no native host is registered, that is, the app
 * is not installed (or its browser integration is off). Chromium and Firefox word it
 * differently.
 */
export function hostMissing(error: string): boolean {
	return /native messaging host not found|no such native application/i.test(error);
}
