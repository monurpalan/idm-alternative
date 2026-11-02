// A new version of the app.

import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

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
