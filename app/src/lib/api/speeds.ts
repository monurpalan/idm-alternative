// A download's speed over its whole run, for the graph in its details.

import { invoke } from '@tauri-apps/api/core';
import type { TaskId } from '../api.ts';

/** A download's speed from its first byte on: a point per `step` seconds of downloading,
 * bytes per second, oldest first; `count` seconds of the next point so far, adding up to
 * `sum`. At most `SPEED_POINTS` points: then neighbours merge and the step doubles. */
export interface SpeedHistory {
	step: number;
	points: number[];
	count: number;
	sum: number;
}

export const SPEED_POINTS = 300;

/** Asked when the details show; null for a download that never began. */
export const speedHistory = (id: TaskId) => invoke<SpeedHistory | null>('speed_history', { id });

/** `h` with one more second at `speed`, as the manager keeps it while the download runs. */
export function pushSpeed(h: SpeedHistory, speed: number): SpeedHistory {
	let { step, points, count, sum } = h;
	sum += Math.max(0, speed);
	count += 1;
	if (count >= step) {
		points = [...points, Math.round(sum / count)];
		count = 0;
		sum = 0;
		if (points.length >= SPEED_POINTS) {
			const merged: number[] = [];
			for (let i = 0; i < points.length; i += 2) merged.push(Math.floor((points[i] + (points[i + 1] ?? points[i])) / 2));
			points = merged;
			step *= 2;
		}
	}
	return { step, points, count, sum };
}
