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
	/** What the failure needs from the user, when it is something they can fix. */
	error_kind: 'auth' | 'expired' | 'changed' | 'disk' | 'checksum' | null;
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
	/** The server asks for a user name and password first. */
	needs_login: boolean;
	realm: string | null;
}

export interface Login {
	user: string;
	password: string;
}

export interface Settings {
	dir: string;
	per_category: boolean;
	max_active: number;
	connections: number;
	overwrite: boolean;
	/** All downloads together, bytes per second. */
	speed_limit: number | null;
	browser: string;
	proxy: string;
	close_to_tray: boolean;
	start_with_windows: boolean;
	notify: boolean;
	browser_integration: boolean;
}

/** List changes; progress comes separately, batched (see `onManagerEvent`). */
export type ManagerEvent =
	| { type: 'added'; data: Task }
	| { type: 'updated'; data: Task }
	| { type: 'removed'; data: TaskId };

export const listTasks = () => invoke<Task[]>('list_tasks');
export type Headers = [string, string][];

export const probe = (url: string, login: Login | null = null, headers: Headers | null = null) =>
	invoke<Probe>('probe', { url, login, headers });
export const addTask = (request: {
	url: string;
	name: string | null;
	dir: string | null;
	start: boolean;
	login: Login | null;
	headers?: Headers | null;
}) => invoke<Task>('add_task', { request });

/** A download the browser handed over. */
export interface Capture {
	url: string;
	filename: string | null;
	size: number | null;
	/** Cookies, referrer, user agent: what the browser would have sent. */
	headers: Headers;
}

export interface CapturedLinks {
	referrer: string | null;
	links: { url: string; text: string }[];
}

/** Video or audio the extension saw a page play. */
export interface CapturedMedia {
	page: string | null;
	title: string | null;
	items: {
		url: string;
		kind: 'hls' | 'dash' | 'video' | 'audio' | 'page';
		size: number | null;
		mime: string | null;
	}[];
	headers: Headers;
}

export const onCapturedMedia = (handler: (m: CapturedMedia) => void): Promise<UnlistenFn> =>
	listen<CapturedMedia>('capture-media', (e) => handler(e.payload));

export const onCapture = (handler: (c: Capture) => void): Promise<UnlistenFn> =>
	listen<Capture>('capture', (e) => handler(e.payload));
export const onCapturedLinks = (handler: (l: CapturedLinks) => void): Promise<UnlistenFn> =>
	listen<CapturedLinks>('capture-links', (e) => handler(e.payload));
export const setLogin = (id: TaskId, login: Login) => invoke<void>('set_login', { id, login });
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
/** Opens the download's page in the browser; the next captured download continues this one. */
export const refreshInBrowser = (id: TaskId) => invoke<void>('refresh_in_browser', { id });
/** Short messages for the status bar (e.g. a link refreshed from the browser). */
export const onNotice = (handler: (text: string) => void): Promise<UnlistenFn> =>
	listen<string>('notice', (e) => handler(e.payload));
export const getSettings = () => invoke<Settings>('get_settings');
/** Checks, applies and saves; returns what was saved. */
export const setSettings = (settings: Settings) => invoke<Settings>('set_settings', { settings });

/** Settings changed (from the dialog or the tray). */
export const onSettings = (handler: (s: Settings) => void): Promise<UnlistenFn> =>
	listen<Settings>('settings', (e) => handler(e.payload));

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
