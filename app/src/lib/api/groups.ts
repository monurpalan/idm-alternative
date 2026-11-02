// Downloads added together: a playlist, a batch, the files of a site.

import { invoke } from '@tauri-apps/api/core';

/** Downloads added together; the list folds them into one row. */
export interface Group {
	id: number;
	name: string;
	added_at: number;
}

export const listGroups = () => invoke<Group[]>('list_groups');
/** A group for downloads about to be added together (give its id to `addTask`). */
export const newGroup = (name: string) => invoke<Group>('new_group', { name });
