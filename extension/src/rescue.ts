// Downloads the browser left interrupted, for the popup to offer to Shardload: the app takes
// over the browser's partial file when the server still serves the same file.

/** What the popup needs of a browser download (Chromium's and Firefox's `DownloadItem`). */
export interface Interrupted {
	id: number;
	url: string;
	finalUrl?: string;
	referrer?: string;
	/** The full path the browser was saving to. */
	filename: string;
	state: string;
	error?: string;
	bytesReceived: number;
	totalBytes: number;
}

/**
 * Whether the popup offers it: interrupted by the network, the server or a full disk, not
 * cancelled by the user (the extension cancels the downloads it takes over, too), and from
 * the web.
 */
export function rescuable(item: Interrupted): boolean {
	return (
		item.state === 'interrupted' &&
		item.error !== 'USER_CANCELED' &&
		item.error !== 'USER_SHUTDOWN' &&
		/^https?:/i.test(item.finalUrl || item.url) &&
		item.filename !== ''
	);
}

/** The message for the app. */
export function rescueMessage(item: Interrupted, cookies: string | null, userAgent: string) {
	return {
		type: 'rescue',
		url: item.finalUrl || item.url,
		referrer: item.referrer || null,
		size: item.totalBytes > 0 ? item.totalBytes : null,
		target: item.filename,
		received: Math.max(0, item.bytesReceived),
		cookies,
		user_agent: userAgent
	};
}

/** The file name out of a full path (either kind of separator). */
export function fileName(path: string): string {
	return path.split(/[\\/]/).pop() || path;
}
