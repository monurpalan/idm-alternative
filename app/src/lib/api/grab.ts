// The site grabber: the files a site's pages link to.

import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

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
