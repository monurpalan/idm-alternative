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
	/** The site's saved login answered: its user name. */
	saved_login: string | null;
	realm: string | null;
}

export interface Login {
	user: string;
	password: string;
	/** Save it for the site, so its downloads log in by themselves. */
	remember?: boolean;
}

/** A site with a remembered login (the password stays in the app). */
export interface SavedLogin {
	site: string;
	user: string;
}

/** A batch address (`photo[001-100].jpg`): how many downloads, the first and the last. */
export interface Batch {
	count: number;
	first: string;
	last: string;
}

export const batchPreview = (url: string) => invoke<Batch | null>('batch_preview', { url });
/** Adds every download of a batch address; returns how many. */
export const addBatch = (request: {
	url: string;
	dir: string | null;
	start: boolean;
	login: Login | null;
	headers?: Headers | null;
}) => invoke<number>('add_batch', { request: { ...request, name: null, stream: null } });

/** The interface's language now: the setting, or Windows' for 'system'. */
export const uiLanguage = () => invoke<'en' | 'tr'>('ui_language');

/** Opens the folder with the app's log files. */
export const openLogs = () => invoke<void>('open_logs');
/** Saves the settings to a JSON file (saved logins stay out). */
export const exportSettings = (path: string) => invoke<void>('export_settings', { path });
/** Applies a settings file; returns the settings now in force. */
export const importSettings = (path: string) => invoke<Settings>('import_settings', { path });

/** Opens the small progress window of a download. */
export const showProgress = (id: TaskId) => invoke<void>('show_progress', { id });

export const savedLogins = () => invoke<SavedLogin[]>('saved_logins');
export const forgetLogin = (site: string) => invoke<void>('forget_login', { site });

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
	update_tools: boolean;
	check_updates: boolean;
	schedule: Schedule;
	watch_clipboard: boolean;
	virus_scan: boolean;
	scanner: string;
	progress_windows: boolean;
	/** 'system' follows Windows' display language. */
	language: 'system' | 'en' | 'tr';
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
	/** A stream: `url` is then the chosen video playlist, or with `ytdlp` (a format
	 * selector) the page yt-dlp downloads from. */
	stream?: {
		audio: string | null;
		ytdlp?: string | null;
		dash?: string | null;
		/** Saved next to the video as `<name>.<language>.vtt`. */
		subtitles?: { id: string; language: string | null }[];
	} | null;
}) => invoke<Task>('add_task', { request });

/** What a page of a site like YouTube offers (read by yt-dlp). */
export interface PageInfo {
	title: string | null;
	duration: number | null;
	heights: number[];
	audio_only: boolean;
}

export const inspectPage = (url: string, headers: Headers) =>
	invoke<PageInfo>('inspect_page', { url, headers });

/** yt-dlp's format selector for a choice; the same as `idma_manager::ytdlp::selector`. */
export function ytdlpSelector(height: number | null): string {
	return height === null
		? 'bestaudio/best'
		: `bestvideo[height<=${height}]+bestaudio/best[height<=${height}]/best`;
}

/** Helper programs the app has; versions of the ones it sets up itself. */
export interface ToolStatus {
	ffmpeg: boolean;
	ytdlp: string | null;
	deno: string | null;
}

export const tools = () => invoke<ToolStatus>('tools');
/** Sets up (or updates) yt-dlp or deno; returns the new version. */
export const installTool = (tool: 'ytdlp' | 'deno') => invoke<string>('install_tool', { tool });

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

/** What a stream offers; see `idma-media`. */
export interface StreamInfo {
	format: 'hls' | 'dash';
	video: {
		id: string;
		bandwidth: number;
		width: number | null;
		height: number | null;
		codecs: string | null;
		frame_rate: number | null;
		audio_group: string | null;
	}[];
	audio: StreamTrack[];
	subtitles: StreamTrack[];
	duration: number | null;
	live: boolean;
	encryption: string | null;
}

export interface StreamTrack {
	id: string | null;
	name: string | null;
	language: string | null;
	group: string | null;
	default: boolean;
	bandwidth: number | null;
	codecs: string | null;
}

export const inspectMedia = (url: string, headers: Headers) =>
	invoke<StreamInfo>('inspect_media', { url, headers });

export const onCapturedMedia = (handler: (m: CapturedMedia) => void): Promise<UnlistenFn> =>
	listen<CapturedMedia>('capture-media', (e) => handler(e.payload));

export interface Schedule {
	enabled: boolean;
	/** `HH:MM`, empty for none. */
	start: string;
	stop: string;
	/** Monday first. */
	days: boolean[];
}

export type WhenDone = 'nothing' | 'exit' | 'sleep' | 'hibernate' | 'shutdown';

export const getWhenDone = () => invoke<WhenDone>('get_when_done');
export const setWhenDone = (action: WhenDone) => invoke<void>('set_when_done', { action });
/** Stops the countdown; with `now`, does that action right away instead. */
export const endCountdown = (now: WhenDone | null = null) => invoke<void>('end_countdown', { now });
export const onWhenDone = (handler: (a: WhenDone) => void): Promise<UnlistenFn> =>
	listen<WhenDone>('when-done', (e) => handler(e.payload));
export const onCountdown = (
	handler: (c: { action: WhenDone; seconds: number }) => void
): Promise<UnlistenFn> =>
	listen<{ action: WhenDone; seconds: number }>('countdown', (e) => handler(e.payload));

export interface AppUpdate {
	version: string;
	notes: string | null;
}

export const checkUpdate = () => invoke<AppUpdate | null>('check_update');
/** Downloads and installs the new version; the app closes and the new one starts. */
export const installUpdate = () => invoke<void>('install_update');
/** The daily check found a new version of the app. */
export const onUpdate = (handler: (u: AppUpdate) => void): Promise<UnlistenFn> =>
	listen<AppUpdate>('update', (e) => handler(e.payload));

/** Tells the app the page listens, so it sends what came while the window was closed. */
export const pageReady = () => invoke<void>('page_ready');

/** A download link was copied (while watching the clipboard is on). */
export const onClipboard = (handler: (url: string) => void): Promise<UnlistenFn> =>
	listen<string>('clipboard', (e) => handler(e.payload));

/** yt-dlp or deno was updated in the background. */
export const onToolsUpdated = (handler: () => void): Promise<UnlistenFn> =>
	listen('tools', () => handler());

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
