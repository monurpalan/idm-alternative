// The main window's right-click menu on a download, and which downloads can start or pause.

import {
	fileOrigin,
	moveTask,
	openFile,
	restartTask,
	showInFolder,
	showProgress,
	type Origin,
	type Task,
	type TaskId
} from './api.ts';
import type { Item } from './components/ContextMenu.svelte';
import { moveToFolder } from './downloads.svelte.ts';
import { msg } from './i18n.svelte.ts';

export const canStart = (t: Task) => t.status === 'paused' || t.status === 'error';
export const canPause = (t: Task) => t.status === 'downloading' || t.status === 'queued';

/** What the menu's items do that the window itself handles. */
export interface MenuActions {
	/** Runs an action, showing its error instead of losing it. */
	run: (p: Promise<unknown>) => unknown;
	/** Resumes the selected downloads that can start. */
	resume: () => unknown;
	/** Pauses the selected downloads that can pause. */
	pause: () => unknown;
	rename: (task: Task) => void;
	trace: (origin: Origin | null) => void;
	remove: (ids: TaskId[]) => void;
}

/** The menu for `task`, right-clicked with `sel` selected. */
export function taskMenu(task: Task, sel: Task[], a: MenuActions): Item[] {
	const one = sel.length === 1 ? sel[0] : null;
	const run = a.run;
	return [
		{
			label: msg('Open'),
			action: () => run(openFile(task.id)),
			disabled: !one || one.status !== 'done'
		},
		{
			label: msg('Show in folder'),
			action: () => run(showInFolder(task.id)),
			disabled: !one || !one.path
		},
		{
			label: msg('Show progress window'),
			action: () => run(showProgress(task.id)),
			disabled: !one
		},
		{
			label: msg('Resume'),
			action: () => a.resume(),
			disabled: !sel.some(canStart),
			separated: true
		},
		{
			label: msg('Pause'),
			action: () => a.pause(),
			disabled: !sel.some(canPause)
		},
		{
			label: msg('Move to top'),
			action: () => run(moveTask(task.id, 'top')),
			disabled: !one || one.status === 'done',
			separated: true
		},
		{
			label: msg('Move to bottom'),
			action: () => run(moveTask(task.id, 'bottom')),
			disabled: !one || one.status === 'done'
		},
		{
			label: msg('Rename…'),
			action: () => a.rename(task),
			disabled: !one,
			shortcut: 'F2',
			separated: true
		},
		{
			label: msg('Move…'),
			action: () => run(moveToFolder(task)),
			disabled: !one
		},
		{
			label: msg('Copy link'),
			action: () => run(navigator.clipboard.writeText(sel.map((t) => t.url).join('\n'))),
			separated: true
		},
		{
			label: msg('Where did it come from?'),
			action: () => run(fileOrigin(task.path ?? '').then((origin) => a.trace(origin))),
			disabled: !one || one.status !== 'done' || !one.path
		},
		{
			label: msg('Download again'),
			action: () => run(restartTask(task.id)),
			disabled: !one
		},
		{
			label: msg('Remove…'),
			action: () => a.remove(sel.map((t) => t.id)),
			danger: true,
			separated: true
		}
	];
}
