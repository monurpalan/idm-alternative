// "Where did this file come from?": a file traced to its download, its checksum checked
// again, and its address asked whether there is something newer.

import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

/** Where a file came from (see `fileOrigin`). */
export interface Origin {
	path: string;
	size: number;
	/** `path`: its download saved it there; `content`: a download of the same bytes;
	 * `mark`: not downloaded here, but the browser marked it; `unknown`: nothing says. */
	traced: 'path' | 'content' | 'mark' | 'unknown';
	url: string | null;
	page: string | null;
	downloaded_at: number | null;
	checksum: string | null;
}

export interface ChecksumCheck {
	expected: string | null;
	/** `download`, or where the publisher posts it (e.g. `SHA256SUMS`). */
	from: string | null;
	actual: string | null;
	matches: boolean | null;
}

export interface UpdateCheck {
	size: number | null;
	etag: string | null;
	last_modified: string | null;
	/** The address serves something else now; null when it cannot tell. */
	changed: boolean | null;
	release: string | null;
	latest_release: string | null;
	/** The server has a `.zsync`: downloading again takes the unchanged parts from this copy. */
	delta: boolean;
}

export const fileOrigin = (path: string) => invoke<Origin>('file_origin', { path });
/** A dropped file, known by its size and SHA-256 (or name): the page gets no path. */
export const droppedFileOrigin = (size: number, sha256: string | null, name: string) =>
	invoke<Origin | null>('dropped_file_origin', { size, sha256, name });
export const checkFileChecksum = (path: string, checksum: string | null, url: string | null) =>
	invoke<ChecksumCheck>('check_file_checksum', { path, checksum, url });
export const checkForUpdate = (url: string, size: number, downloadedAt: number | null) =>
	invoke<UpdateCheck>('check_for_update', { url, size, downloadedAt });
/** "Where did a file come from?" was chosen in the tray menu. */
export const onOriginPick = (handler: () => void): Promise<UnlistenFn> =>
	listen('origin-pick', () => handler());
