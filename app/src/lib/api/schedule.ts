// The scheduler, and what to do once every download is done.

import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

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
