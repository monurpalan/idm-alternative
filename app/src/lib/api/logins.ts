// Sign-ins: a user name and password for a download, and the ones saved per site.

import { invoke } from '@tauri-apps/api/core';

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

export const savedLogins = () => invoke<SavedLogin[]>('saved_logins');
export const forgetLogin = (site: string) => invoke<void>('forget_login', { site });
