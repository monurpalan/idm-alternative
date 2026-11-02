// "You downloaded this before": an earlier download of what is about to be added, and using
// its file instead.

import { invoke } from '@tauri-apps/api/core';
import type { Headers, Status, Task, TaskId } from '../api.ts';

export interface Previous {
	/** `url`: the same address; `name_size`: another one, but the same name and size;
	 * `content`: the same content, by the checksum the new download is expected to have. */
	matched: 'url' | 'name_size' | 'content';
	/** On the list: its id and status (not `done`: still to finish). */
	id: TaskId | null;
	status: Status | null;
	/** Its history entry, once finished: for opening the file. */
	history: number | null;
	name: string | null;
	path: string | null;
	size: number | null;
	finished_at: number | null;
	/** The finished file is still where it was saved. */
	on_disk: boolean;
	/** On the drive the new download would go to: a hard link can stand in for it. */
	linkable: boolean;
}

/** `finalUrl`, `name` and `size` are what the server said (see `probe`); `checksum` the one
 * shown on the page, `dir` where it would go. */
export const findPrevious = (
	url: string,
	finalUrl: string | null = null,
	name: string | null = null,
	size: number | null = null,
	more: { checksum?: string | null; headers?: Headers | null; dir?: string | null } = {}
) =>
	invoke<Previous | null>('find_previous', {
		url,
		finalUrl,
		name,
		size,
		checksum: more.checksum ?? null,
		headers: more.headers ?? null,
		dir: more.dir ?? null
	});
/** Puts the earlier download's file into `dir` as `name`, copied or hard-linked. */
export const reusePrevious = (history: number, dir: string | null, name: string | null, how: 'copy' | 'link') =>
	invoke<Task>('reuse_previous', { history, dir, name, how });
/** For each link, whether it was downloaded before or is on the list. */
export const knownLinks = (urls: string[]) => invoke<boolean[]>('known_links', { urls });
export const openPrevious = (history: number) => invoke<void>('open_previous', { history });
export const showPrevious = (history: number) => invoke<void>('show_previous', { history });
/** Forgets the finished downloads; their files stay. */
export const clearHistory = () => invoke<void>('clear_history');
