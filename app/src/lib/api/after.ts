// What happens when a download finishes: its own wish (unpack, move) and the user's rules,
// which alone may run a command or call a webhook.

import type { Category } from '../api.ts';

/** What one download asks for once finished. */
export interface After {
	unpack: boolean;
	move_to: string | null;
}

/** A rule from the settings: every condition given must hold. */
export interface Rule {
	category: Category | null;
	/** The link's host or a domain above it. */
	site: string | null;
	/** File name pattern with `*` and `?`. */
	name: string | null;
	unpack: boolean;
	move_to: string | null;
	/** A program and its arguments, no shell; `{file}` and `{dir}` are filled in. */
	command: string | null;
	webhook: string | null;
}

/** How one action went. */
export interface AfterDone {
	action: 'move' | 'unpack' | 'command' | 'webhook';
	ok: boolean;
	/** The folder, the program, the webhook's address; or why it could not be done at all. */
	detail: string;
	/** Files unpacked, the exit code, the HTTP status; null when it could not be done. */
	count: number | null;
	/** Archive entries left out. */
	left_out: number;
}

/** A name of an archive the app can unpack: ZIP or tar, plain or with gzip or xz. */
export const canUnpack = (name: string | null | undefined) => /\.(zip|tar|tgz|txz|tar\.gz|tar\.xz)$/i.test(name ?? '');
