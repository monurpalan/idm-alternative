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

export interface Probe {
	url: string;
	name: string;
	size: number | null;
	resumable: boolean;
}

export interface Settings {
	dir: string;
	max_active: number;
}

/** List changes; progress comes separately, batched (see `onManagerEvent`). */
export type ManagerEvent =
	| { type: 'added'; data: Task }
	| { type: 'updated'; data: Task }
	| { type: 'removed'; data: TaskId };

export const listTasks = () => invoke<Task[]>('list_tasks');
export const probe = (url: string) => invoke<Probe>('probe', { url });
export const addTask = (request: {
	url: string;
	name: string | null;
	dir: string | null;
	start: boolean;
}) => invoke<Task>('add_task', { request });
export const startTasks = (ids: TaskId[]) => invoke<void>('start_tasks', { ids });
export const pauseTasks = (ids: TaskId[]) => invoke<void>('pause_tasks', { ids });
export const removeTasks = (ids: TaskId[], deleteFiles: boolean) =>
	invoke<void>('remove_tasks', { ids, deleteFiles });
export const startAll = () => invoke<void>('start_all');
export const pauseAll = () => invoke<void>('pause_all');
export const moveTask = (id: TaskId, to: 'top' | 'up' | 'down' | 'bottom') =>
	invoke<void>('move_task', { id, to });
export const restartTask = (id: TaskId) => invoke<void>('restart_task', { id });
export const openFile = (id: TaskId) => invoke<void>('open_file', { id });
export const showInFolder = (id: TaskId) => invoke<void>('show_in_folder', { id });
export const setSpeedLimit = (id: TaskId, rate: number | null) =>
	invoke<void>('set_speed_limit', { id, rate });
export const refreshLink = (id: TaskId, url: string) => invoke<void>('refresh_link', { id, url });
export const getSettings = () => invoke<Settings>('get_settings');
export const setMaxActive = (maxActive: number) => invoke<void>('set_max_active', { maxActive });

/**
 * Every change to the list, and the progress of all running downloads a few times a second
 * (one batch per tick). `onLagged` runs when events were dropped: reload the list.
 */
export async function onManagerEvent(
	handler: (e: ManagerEvent) => void,
	onProgress: (batch: TaskProgress[]) => void,
	onLagged: () => void
): Promise<UnlistenFn> {
	const stops = await Promise.all([
		listen<ManagerEvent>('manager', (e) => handler(e.payload)),
		listen<TaskProgress[]>('manager-progress', (e) => onProgress(e.payload)),
		listen('manager-lagged', onLagged)
	]);
	return () => stops.forEach((f) => f());
}
