// The Wayback Machine, for a file its server no longer has: only at the user's click.

import { invoke } from '@tauri-apps/api/core';
import type { TaskId } from '../api.ts';

/** An archived copy added to the list. */
export interface ArchivedCopy {
	id: TaskId;
	/** When it was archived: `YYYYMMDDhhmmss`. */
	timestamp: string;
}

/** Opens the Wayback Machine's copies of a download's address in the browser. */
export const searchWayback = (id: TaskId) => invoke<void>('search_wayback', { id });
/** Asks the Wayback Machine once for its closest copy and adds it; null when it has none. */
export const addArchivedCopy = (id: TaskId) => invoke<ArchivedCopy | null>('add_archived_copy', { id });
