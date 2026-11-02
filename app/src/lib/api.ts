// Typed access to the Rust side (`app/src-tauri`), which wraps `shardload-manager`.

import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import type { After, AfterDone, Rule } from './api/after.ts';
import type { Hint } from './api/hints.ts';
import type { Login } from './api/logins.ts';
import type { NetRules } from './api/net.ts';
import type { Schedule } from './api/schedule.ts';
import type { Structure } from './api/structure.ts';

export type TaskId = number;
export type Status = 'queued' | 'downloading' | 'paused' | 'done' | 'error';
export type Category = 'general' | 'compressed' | 'documents' | 'music' | 'programs' | 'video';
/** `settingup`: ffmpeg is being set up first; the sizes are its download's. `recording`: a
 * live stream, with no size, until it ends or is stopped. */
export type Phase = 'probing' | 'settingup' | 'downloading' | 'verifying' | 'recording' | 'done';

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
	/** How a stream, a page or a torrent is downloaded; only what the list shows is typed. */
	stream: { record?: Recording | null } | null;
	/** What the finished file's first bytes say it is: `program:exe`, `document:pdf`... */
	content: string | null;
	/** The file may not be what its name says. */
	warning: Warning | null;
	/** Unpack or move it once finished, and how that and the rules went. */
	after: After | null;
	after_log: AfterDone[];
	/** Without a checksum: how it checked out by its own structure. */
	structure: Structure | null;
}

/** A finished file that may not be what it says: a program named like a document
 * (`disguised`), or a web page saved where the name promised a file (`webpage`); found by
 * its `name` alone or by its `content`. */
export interface Warning {
	kind: 'disguised' | 'webpage';
	by: 'name' | 'content';
}

/** How a live stream is recorded. */
export interface Recording {
	/** Stop after this many seconds. */
	limit: number | null;
	/** From the start of the broadcast (sites yt-dlp knows that keep it). */
	from_start: boolean;
	/** Start then (Unix milliseconds), not at once. */
	start_at: number | null;
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
	/** While recording a live stream: seconds recorded. */
	recorded: number;
	/** Seconds left, `[soonest, latest]`; null while calculating. */
	eta: [number, number] | null;
	/** What holds the download back, when known for certain. */
	hints: Hint[];
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

/** The interface's language now: the setting, or Windows' for 'system'. */
export const uiLanguage = () => invoke<'en' | 'tr'>('ui_language');

/** Opens the folder with the app's log files. */
export const openLogs = () => invoke<void>('open_logs');
/** Saves the settings to a JSON file (saved logins stay out); true when the proxy's password
 * was left out too. */
export const exportSettings = (path: string) => invoke<boolean>('export_settings', { path });
/** Applies a settings file; returns the settings now in force. */
export const importSettings = (path: string) => invoke<Settings>('import_settings', { path });

/** Opens the small progress window of a download. */
export const showProgress = (id: TaskId) => invoke<void>('show_progress', { id });

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
	/** How fast its downloads went, bytes per second. */
	speed: number | null;
	seen_at: number;
}

export const sites = () => invoke<SiteMemory[]>('sites');
export const siteMemory = (url: string) => invoke<SiteMemory | null>('site_memory', { url });

/** A waiting download whose link, by how long links of its site lasted, may soon stop working. */
export interface LinkWarning {
	id: TaskId;
	/** When the link is expected to stop working, Unix ms. */
	expires_at: number;
	/** Already past that: it most likely needs a fresh link. */
	expired: boolean;
}

export const linkWarnings = () => invoke<LinkWarning[]>('link_warnings');
export const forgetSite = (site: string) => invoke<void>('forget_site', { site });


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
	/** Check downloads without a checksum by their own structure (ZIP, PDF, video…). */
	check_structure: boolean;
	after_rules: Rule[];
	net_rules: NetRules;
}

/** List changes; progress comes separately, batched (see `onManagerEvent`). */
export type ManagerEvent =
	| { type: 'added'; data: Task }
	| { type: 'updated'; data: Task }
	| { type: 'removed'; data: TaskId }
	/** Files being copied to another drive; an `updated` follows when they are in place. */
	| { type: 'moving'; data: { id: TaskId; copied: number; total: number } }
	/** A drive came within the reserve of full: the downloads writing to it were paused. */
	| { type: 'disklow'; data: DiskLow };

export interface DiskLow {
	volume: string;
	free: number;
	paused: TaskId[];
}

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
		/** Only this part of the video; needs ffmpeg (set up by itself). */
		clip?: Clip | null;
		/** A live stream: recorded. */
		record?: Recording | null;
		/** A torrent: indices of the files to download, all when null. */
		torrent?: { files: number[] | null } | null;
		/** Files out of a ZIP archive (`url`), by name, into a folder named after it. */
		archive?: { entries: string[] } | null;
	} | null;
	/** Other addresses of the same file. */
	mirrors?: string[];
	/** One of a group added together (see `newGroup`). */
	group?: number | null;
	/** The title of the page the link came from: names a file whose name says nothing. */
	title?: string | null;
	/** A checksum shown next to the link on that page; checked when the publisher posts none. */
	page_checksum?: string | null;
	/** The size the server said when asked, for the disk check before it starts. */
	size?: number | null;
	after?: After | null;
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

/** A part of a video, in seconds; `exact` cuts at those frames (re-encoding). */
export interface Clip {
	start: number;
	end: number;
	exact: boolean;
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
	/** A live broadcast now: it is recorded. */
	live: boolean;
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
	/** The title of the page the download started from. */
	title: string | null;
	/** A checksum the page shows next to the link, `sha256:<hex>`. */
	checksum: string | null;
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

/** What a pasted `curl`, `wget` or `Invoke-WebRequest` command asks for. */
export interface Pasted {
	url: string;
	headers: Headers;
	/** `-o name` and the like. */
	name: string | null;
	/** It sends data (POST): downloaded with GET, which may not give the same answer. */
	post: boolean;
	/** Headers the download sets itself, left out. */
	dropped: string[];
}
export const parseCommand = (text: string) => invoke<Pasted>('parse_command', { text });
/** `Name: value` lines, checked. */
export const parseHeaders = (text: string) => invoke<Headers>('parse_headers', { text });
/** The headers a download is sent with from now on, as `Name: value` lines. */
export const setHeaders = (id: TaskId, text: string) => invoke<Task>('set_headers', { id, text });
/** Headers as `Name: value` lines, for editing. */
export const headerLines = (headers: Headers) => headers.map(([k, v]) => `${k}: ${v}`).join('\n');
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

/** What downloads about to run need on one drive. */
export interface DriveNeed {
	/** `D:\`, or a mount point. */
	volume: string;
	/** Bytes still to download of the ones with a known size. */
	need: number;
	free: number | null;
	/** How many have no known size yet. */
	unknown: number;
	tasks: TaskId[];
	/** Bytes missing to fit them and keep `reserve` free; null when they fit. */
	short: number | null;
	reserve: number;
}
/** Per drive, what `ids` (and the running downloads) need against the space free. */
export const diskNeeds = (ids: TaskId[]) => invoke<DriveNeed[]>('disk_needs', { ids });
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

export * from './api/extensions.ts';
export * from './api/after.ts';
export * from './api/hints.ts';
export * from './api/logins.ts';
export * from './api/net.ts';
export * from './api/schedule.ts';
export * from './api/previous.ts';
export * from './api/structure.ts';

