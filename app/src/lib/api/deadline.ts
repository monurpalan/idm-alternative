// Downloads wanted finished by a time: their own deadline, or their link's expiry.

import { invoke } from '@tauri-apps/api/core';
import type { TaskId } from '../api.ts';

export interface DeadlineView {
	id: TaskId;
	/** Unix ms. */
	at: number;
	/** `own`: the user's; `link`: when its link is expected to stop working. */
	source: 'own' | 'link';
	standing: 'on_time' | 'at_risk' | 'late';
	/** Bytes per second it needs from now on, when its size is known. */
	needed: number | null;
}

export const deadlines = () => invoke<DeadlineView[]>('deadlines');
/** `at` Unix ms; null clears it. */
export const setDeadline = (id: TaskId, at: number | null) => invoke<void>('set_deadline', { id, at });
