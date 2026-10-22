// The download list as the UI sees it: tasks from the manager, kept current by its events,
// plus the live progress of running downloads.

import { open as pickFolder } from '@tauri-apps/plugin-dialog';
import { SvelteSet } from 'svelte/reactivity';
import { msg } from './i18n.svelte.ts';
import {
	listGroups,
	listTasks,
	onManagerEvent,
	relocateTask,
	type Category,
	type Group,
	type Task,
	type TaskId,
	type TaskProgress
} from './api.ts';

export type View = 'all' | Category | 'unfinished' | 'finished';

export const VIEWS: { id: View; label: string }[] = [
	{ id: 'all', label: msg('All downloads') },
	{ id: 'compressed', label: msg('Compressed') },
	{ id: 'documents', label: msg('Documents') },
	{ id: 'music', label: msg('Music') },
	{ id: 'programs', label: msg('Programs') },
	{ id: 'video', label: msg('Video') },
	{ id: 'unfinished', label: msg('Unfinished') },
	{ id: 'finished', label: msg('Finished') }
];

export type SortKey =
	| 'queue'
	| 'name'
	| 'size'
	| 'progress'
	| 'speed'
	| 'eta'
	| 'status'
	| 'added';

/** A row: the stored task with live numbers folded in. */
export interface Row {
	task: Task;
	name: string;
	size: number | null;
	received: number;
	/** 0..1, or null when the size is unknown. */
	fraction: number | null;
	speed: number;
	/** Seconds, or null. */
	eta: number | null;
	live: TaskProgress | undefined;
}

export function inView(t: Task, view: View): boolean {
	switch (view) {
		case 'all':
			return true;
		case 'unfinished':
			return t.status !== 'done';
		case 'finished':
			return t.status === 'done';
		default:
			return t.category === view;
	}
}

/** Until the server names the file: the last part of the URL path, like the engine does. */
export function nameFromUrl(url: string): string {
	try {
		const segment = new URL(url).pathname.split('/').filter(Boolean).pop();
		return segment ? decodeURIComponent(segment) : url;
	} catch {
		return url;
	}
}

export function row(task: Task, live: TaskProgress | undefined): Row {
	const size = live?.size ?? task.size;
	const received = task.status === 'done' ? (size ?? task.received) : (live?.received ?? task.received);
	const speed = task.status === 'downloading' ? (live?.speed ?? 0) : 0;
	return {
		task,
		name: task.name ?? nameFromUrl(task.url),
		size,
		received,
		fraction: size ? Math.min(1, received / size) : task.status === 'done' ? 1 : null,
		speed,
		eta: size && speed > 0 ? (size - received) / speed : null,
		live
	};
}

const STATUS_ORDER = { downloading: 0, queued: 1, paused: 2, error: 3, done: 4 } as const;

export function compare(a: Row, b: Row, key: SortKey): number {
	switch (key) {
		case 'queue':
			// Unfinished ones in queue order, finished ones after them.
			return (
				Number(a.task.status === 'done') - Number(b.task.status === 'done') ||
				a.task.queue_pos - b.task.queue_pos
			);
		case 'name':
			return a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: 'base' });
		case 'size':
			return (a.size ?? -1) - (b.size ?? -1);
		case 'progress':
			return (a.fraction ?? -1) - (b.fraction ?? -1);
		case 'speed':
			return a.speed - b.speed;
		case 'eta':
			return (a.eta ?? Infinity) - (b.eta ?? Infinity);
		case 'status':
			return STATUS_ORDER[a.task.status] - STATUS_ORDER[b.task.status];
		case 'added':
			return a.task.added_at - b.task.added_at || a.task.queue_pos - b.task.queue_pos;
	}
}

/** Speed samples kept per running download: a minute at four per second. */
export const HISTORY = 240;

class Downloads {
	tasks = $state<Task[]>([]);
	live = $state<Record<TaskId, TaskProgress>>({});
	/** Downloads whose files are being copied to another drive: the share done, 0 to 1. */
	moving = $state<Record<TaskId, number>>({});
	/**
	 * Recent speeds per download, oldest first. Not reactive itself: whoever draws it also
	 * reads `live[id]`, which changes with every new sample.
	 */
	history = new Map<TaskId, number[]>();
	selected = new SvelteSet<TaskId>();
	/** Groups of downloads added together, by id. */
	groups = $state<Record<number, Group>>({});
	/** Groups shown opened in the list; the others are folded into one row. */
	opened = new SvelteSet<number>();
	loaded = $state(false);
	error = $state<string | null>(null);

	async reload() {
		try {
			const [tasks] = await Promise.all([listTasks(), this.reloadGroups()]);
			this.tasks = tasks;
			this.loaded = true;
		} catch (e) {
			this.error = String(e);
		}
	}

	async reloadGroups() {
		const groups = await listGroups();
		this.groups = Object.fromEntries(groups.map((g) => [g.id, g]));
	}

	/** Starts listening; returns the function that stops it. */
	connect(): () => void {
		const stop = onManagerEvent(
			(e) => {
				switch (e.type) {
					case 'added':
						if (!this.tasks.some((t) => t.id === e.data.id)) this.tasks.push(e.data);
						if (e.data.group_id !== null && !this.groups[e.data.group_id])
							this.reloadGroups().catch(() => {});
						break;
					case 'updated': {
						const i = this.tasks.findIndex((t) => t.id === e.data.id);
						if (i >= 0) this.tasks[i] = e.data;
						else this.tasks.push(e.data);
						if (e.data.status !== 'downloading') delete this.live[e.data.id];
						delete this.moving[e.data.id];
						break;
					}
					case 'moving':
						this.moving[e.data.id] = e.data.total ? e.data.copied / e.data.total : 0;
						break;
					case 'removed':
						this.tasks = this.tasks.filter((t) => t.id !== e.data);
						delete this.live[e.data];
						this.history.delete(e.data);
						this.selected.delete(e.data);
						break;
				}
			},
			(batch) => {
				for (const p of batch) {
					// Ignore progress that arrives after the download stopped.
					if (this.tasks.find((t) => t.id === p.id)?.status === 'downloading') {
						const h = this.history.get(p.id) ?? [];
						h.push(p.speed);
						if (h.length > HISTORY) h.splice(0, h.length - HISTORY);
						this.history.set(p.id, h);
						this.live[p.id] = p;
					}
				}
			},
			() => this.reload()
		);
		this.reload();
		return () => {
			stop.then((f) => f());
		};
	}

	/** Total speed of everything running. */
	get speed(): number {
		let sum = 0;
		for (const p of Object.values(this.live)) sum += p.speed;
		return sum;
	}

	get active(): number {
		return this.tasks.filter((t) => t.status === 'downloading').length;
	}
}

export const downloads = new Downloads();

/** Asks for a folder and moves the download there; nothing when the user cancels. */
export async function moveToFolder(task: Task): Promise<void> {
	const dir = await pickFolder({ directory: true, defaultPath: task.dir });
	if (typeof dir === 'string') await relocateTask(task.id, dir);
}
