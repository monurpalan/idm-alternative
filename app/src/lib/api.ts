// Typed access to the Rust side (`app/src-tauri`), which wraps `idma-manager`.

import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

export type TaskId = number;
export type Status = 'queued' | 'downloading' | 'paused' | 'done' | 'error';
export type Category = 'general' | 'compressed' | 'documents' | 'music' | 'programs' | 'video';
export type Phase = 'probing' | 'downloading' | 'verifying' | 'done';

export interface Task {
	id: TaskId;
	url: string;
	headers: [string, string][];
	dir: string;
	name: string | null;
	path: string | null;
	size: number | null;
	received: number;
	status: Status;
	error: string | null;
	category: Category;
	connections: number;
	speed_limit: number | null;
	checksum: string | null;
	added_at: number;
	finished_at: number | null;
	queue_pos: number;
}

export interface TaskProgress {
	id: TaskId;
	phase: Phase;
	size: number | null;
	received: number;
	speed: number;
	connections: number;
	verified: number;
	/** `[start, end, received, active]` per segment. */
	segments: [number, number, number, boolean][];
}

export type ManagerEvent =
	| { type: 'added'; data: Task }
	| { type: 'updated'; data: Task }
	| { type: 'progress'; data: TaskProgress }
	| { type: 'removed'; data: TaskId };

export const listTasks = () => invoke<Task[]>('list_tasks');
export const addTask = (url: string) => invoke<Task>('add_task', { url });
export const startTask = (id: TaskId) => invoke<void>('start_task', { id });
export const pauseTask = (id: TaskId) => invoke<void>('pause_task', { id });
export const removeTask = (id: TaskId, deleteFiles: boolean) =>
	invoke<void>('remove_task', { id, deleteFiles });

/** Every change to the list. `onLagged` runs when events were dropped: reload the list. */
export async function onManagerEvent(
	handler: (e: ManagerEvent) => void,
	onLagged: () => void
): Promise<UnlistenFn> {
	const a = await listen<ManagerEvent>('manager', (e) => handler(e.payload));
	const b = await listen('manager-lagged', onLagged);
	return () => {
		a();
		b();
	};
}
