// Typed access to the Rust side (`app/src-tauri`), which wraps `shardload-manager`.

import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

export type TaskId = number;
export type Status = 'queued' | 'downloading' | 'paused' | 'done' | 'error';
export type Category = 'general' | 'compressed' | 'documents' | 'music' | 'programs' | 'video';
/** `settingup`: ffmpeg is being set up first; the sizes are its download's. */
export type Phase = 'probing' | 'settingup' | 'downloading' | 'verifying' | 'done';

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
	error_kind: 'auth' | 'expired' | 'changed' | 'disk' | 'checksum' | 'offline' | null;
	category: Category;
	connections: number;
	speed_limit: number | null;
	checksum: string | null;
	/** Where `checksum` was found when the user gave none, e.g. `SHA256SUMS`. */
	checksum_from: string | null;
	added_at: number;
	finished_at: number | null;
	queue_pos: number;
	/** Where `url` led after redirects. */
	final_url: string | null;
	/** Added together with others: a playlist, a batch, the files of a site. */
	group_id: number | null;
}

/** Downloads added together; the list folds them into one row. */
export interface Group {
	id: number;
	name: string;
	added_at: number;
}

export const listGroups = () => invoke<Group[]>('list_groups');
/** A group for downloads about to be added together (give its id to `addTask`). */
export const newGroup = (name: string) => invoke<Group>('new_group', { name });

export interface TaskProgress {
	id: TaskId;
	phase: Phase;
	size: number | null;
	received: number;
	speed: number;
	connections: number;
	verified: number;
	/** While verifying: where the checksum was found, when the user gave none. */
	checksum_from: string | null;
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
	/** How many were downloaded before or are on the list. */
	known: number;
}

export const batchPreview = (url: string) => invoke<Batch | null>('batch_preview', { url });
/** Adds every download of a batch address (with `skip_known`, not those downloaded before);
 * returns how many. */
export const addBatch = (request: {
	url: string;
	dir: string | null;
	start: boolean;
	login: Login | null;
	headers?: Headers | null;
	skip_known?: boolean;
}) => invoke<number>('add_batch', { request: { ...request, name: null, stream: null } });

/** An earlier download of what is about to be added. */
export interface Previous {
	/** `url`: the same address; `name_size`: another one, but the same name and size. */
	matched: 'url' | 'name_size';
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
}

/** `finalUrl`, `name` and `size` are what the server said (see `probe`). */
export const findPrevious = (
	url: string,
	finalUrl: string | null = null,
	name: string | null = null,
	size: number | null = null
) => invoke<Previous | null>('find_previous', { url, finalUrl, name, size });
/** For each link, whether it was downloaded before or is on the list. */
export const knownLinks = (urls: string[]) => invoke<boolean[]>('known_links', { urls });
export const openPrevious = (history: number) => invoke<void>('open_previous', { history });
export const showPrevious = (history: number) => invoke<void>('show_previous', { history });
/** Forgets the finished downloads; their files stay. */
export const clearHistory = () => invoke<void>('clear_history');

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

/** What downloads learned about a site, used for its next download. */
export interface SiteMemory {
	site: string;
	/** It refused more connections than this; the next download tries one more. */
	max_connections: number | null;
	/** It sends whole files only. */
	no_ranges: boolean;
	/** The identity it served after refusing the one in the settings (`none`: a plain client). */
	browser: string | null;
	/** Seconds its links worked before expiring. */
	link_lifetime: number | null;
	seen_at: number;
}

export const sites = () => invoke<SiteMemory[]>('sites');
export const siteMemory = (url: string) => invoke<SiteMemory | null>('site_memory', { url });
export const forgetSite = (site: string) => invoke<void>('forget_site', { site });

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
	/** Open magnet links and `.torrent` files with the app. */
	torrent_links: boolean;
	/** Mark finished downloads as coming from the Internet (Windows' Zone.Identifier). */
	mark_downloads: boolean;
	/** Look for a published checksum of downloads that have none, and check it. */
	find_checksums: boolean;
}

/** List changes; progress comes separately, batched (see `onManagerEvent`). */
export type ManagerEvent =
	| { type: 'added'; data: Task }
	| { type: 'updated'; data: Task }
	| { type: 'removed'; data: TaskId }
	/** Files being copied to another drive; an `updated` follows when they are in place. */
	| { type: 'moving'; data: { id: TaskId; copied: number; total: number } };

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
		/** A torrent: indices of the files to download, all when null. */
		torrent?: { files: number[] | null } | null;
		/** Files out of a ZIP archive (`url`), by name, into a folder named after it. */
		archive?: { entries: string[] } | null;
	} | null;
	/** Other addresses of the same file. */
	mirrors?: string[];
	/** One of a group added together (see `newGroup`). */
	group?: number | null;
}) => invoke<Task>('add_task', { request });

/** Video sites whose pages yt-dlp reads (a pasted link opens the page window). */
const VIDEO_SITES = [
	'youtube.com',
	'youtu.be',
	'vimeo.com',
	'dailymotion.com',
	'twitch.tv',
	'tiktok.com',
	'instagram.com',
	'x.com',
	'twitter.com'
];

/** A page of a video site rather than a file: a watch page, a playlist, a channel… */
export function isVideoPage(url: string): boolean {
	try {
		const host = new URL(url.trim()).hostname.replace(/^(www|m|music)\./, '');
		return VIDEO_SITES.some((s) => host === s || host.endsWith(`.${s}`));
	} catch {
		return false;
	}
}

/** A magnet link, or a `.torrent` file on the web or on disk. */
export const isTorrent = (url: string) => {
	const u = url.trim().toLowerCase();
	return u.startsWith('magnet:') || /\.torrent$/.test(u.split(/[?#]/)[0]);
};

/** What a torrent holds; a magnet link asks peers, which can take a while. */
export interface TorrentInfo {
	name: string;
	size: number;
	/** Paths inside the torrent, `/`-separated. */
	files: { name: string; size: number }[];
}

export const inspectTorrent = (url: string) => invoke<TorrentInfo>('inspect_torrent', { url });

/** An address whose name says ZIP archive. */
export const isZip = (url: string, name?: string | null) =>
	/\.zip$/i.test(url.trim().split(/[?#]/)[0]) || /\.zip$/i.test(name ?? '');

/** What a ZIP archive on the server holds, read without downloading it. */
export interface ArchiveInfo {
	name: string;
	size: number;
	zip64: boolean;
	/** Paths inside the archive, `/`-separated; folders end with `/`. */
	entries: {
		name: string;
		size: number;
		compressed_size: number;
		method: string;
		is_dir: boolean;
		encrypted: boolean;
		/** Why it cannot be extracted. */
		unsupported: string | null;
	}[];
}

export const inspectArchive = (url: string, login: Login | null = null, headers: Headers | null = null) =>
	invoke<ArchiveInfo>('inspect_archive', { url, login, headers });

/** A file the site grabber found, with its link text and the page it was on. */
export interface GrabbedFile {
	url: string;
	text: string;
	page: string;
}

/** Reads `url` and the pages it links to, down to `depth`, and lists the files they link to. */
export const grabSite = (url: string, depth: number, sameSite: boolean) =>
	invoke<GrabbedFile[]>('grab_site', { url, depth, sameSite });
export const cancelGrab = () => invoke<void>('cancel_grab');
export const onGrabProgress = (
	handler: (p: { pages: number; files: number }) => void
): Promise<UnlistenFn> => listen<{ pages: number; files: number }>('grab-progress', (e) => handler(e.payload));

/** What a page of a site like YouTube offers (read by yt-dlp). */
export interface PageInfo {
	title: string | null;
	duration: number | null;
	heights: number[];
	audio_only: boolean;
	/** A playlist or a channel: its videos. */
	playlist: Playlist | null;
	/** A video of a playlist: reading it with `wholeList` gives the playlist. */
	in_playlist: boolean;
}

export interface Playlist {
	title: string | null;
	uploader: string | null;
	/** A channel's videos, newest first. */
	channel: boolean;
	entries: { url: string; title: string | null; duration: number | null; thumbnail: string | null }[];
}

export const inspectPage = (url: string, headers: Headers, wholeList = false) =>
	invoke<PageInfo>('inspect_page', { url, headers, wholeList });

/** Adds videos of a playlist, each a download of its own, in one group and into a folder
 * named after the list; `height` null for audio alone. Returns how many. */
export const addPlaylist = (request: {
	title: string;
	entries: { url: string; title: string | null }[];
	height: number | null;
	headers: Headers;
	start: boolean;
}) => invoke<number>('add_playlist', { request });

/** yt-dlp's format selector for a choice; the same as `shardload_manager::ytdlp::selector`. */
export function ytdlpSelector(height: number | null): string {
	return height === null
		? 'bestaudio/best'
		: `bestvideo[height<=${height}]+bestaudio/best[height<=${height}]/best`;
}

/** Helper programs the app has; versions of the ones it sets up itself. */
export interface ToolStatus {
	ffmpeg: string | null;
	/** The ffmpeg used is the one on PATH (its version may be empty). */
	ffmpeg_on_path: boolean;
	ytdlp: string | null;
	deno: string | null;
}

export type ToolId = 'ytdlp' | 'deno' | 'ffmpeg';

export const tools = () => invoke<ToolStatus>('tools');
/** Sets up (or updates) a helper program; returns the new version. */
export const installTool = (tool: ToolId) => invoke<string>('install_tool', { tool });

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

/** What a stream offers; see `shardload-media`. */
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
/** A new file name; the files on disk follow (a running download pauses for a moment). */
export const renameTask = (id: TaskId, name: string) => invoke<Task>('rename_task', { id, name });
/** Another folder; the files on disk follow, copied and checked when it is another drive. */
export const relocateTask = (id: TaskId, dir: string) => invoke<Task>('relocate_task', { id, dir });
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

/** What the app knows of the browser extension in each browser. */
export interface ExtensionStatus {
	browsers: { browser: string; state: 'connected' | 'outdated' | 'missing'; version: string | null }[];
	warning: { kind: 'not_found' } | { kind: 'outdated'; browser: string } | null;
	/** Seconds until this may change by itself. */
	check_in: number | null;
}

export const extensionStatus = () => invoke<ExtensionStatus>('extension_status');
/** "Don't show again" on "Browser extension not found". */
export const dismissExtensionWarning = () => invoke<ExtensionStatus>('dismiss_extension_warning');
/** Opens where to get the extension (for `browser`, or any). */
export const installExtension = (browser?: string) =>
	invoke<void>('install_extension', { browser: browser ?? null });
/** An extension said hello, or browser integration was turned on or off. */
export const onExtensions = (handler: (s: ExtensionStatus) => void): Promise<UnlistenFn> =>
	listen<ExtensionStatus>('extensions', (e) => handler(e.payload));

/** The browsers' names, as the extension and the app call them. */
export const BROWSER_NAMES: Record<string, string> = {
	chrome: 'Chrome',
	edge: 'Edge',
	firefox: 'Firefox',
	brave: 'Brave',
	opera: 'Opera',
	vivaldi: 'Vivaldi',
	chromium: 'Chromium'
};
