// The browser extension as the app knows it.

import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

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
