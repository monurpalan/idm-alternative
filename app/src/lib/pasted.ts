// What is pasted or dropped on the main window.

/** A link the app downloads from. */
export function looksLikeUrl(s: string): boolean {
	return /^(https?|ftps?|ftpes|sftp):\/\/\S+$/i.test(s.trim());
}

/** The event is in a text field, where keys and pastes are the field's. */
export function inField(e: Event): boolean {
	const t = e.target as HTMLElement | null;
	return !!t && (t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.isContentEditable);
}

/** Files above this are looked up by name and size: hashing them in the page would
 * take their whole size in memory. */
const HASH_UP_TO = 256 << 20;

/** A dropped file's SHA-256 in hex, or null when it is too large to hash here. */
export async function sha256(file: File): Promise<string | null> {
	if (file.size > HASH_UP_TO) return null;
	const digest = await crypto.subtle.digest('SHA-256', await file.arrayBuffer());
	return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, '0')).join('');
}
