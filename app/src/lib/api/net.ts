// Network and power: the rules for a metered network, for running on battery and for named
// networks, the monthly data budget, and what the system says now.

import { invoke } from '@tauri-apps/api/core';

export interface NetRules {
	/** On a metered network, videos and torrents wait. */
	metered_hold: boolean;
	/** On battery, all downloads together at most this many bytes per second. */
	battery_limit: number | null;
	/** On the network of this name, downloads go through this proxy. */
	proxies: { network: string; proxy: string }[];
	/** Bytes a month. */
	monthly_budget: number | null;
	/** Past the budget, downloads wait; otherwise it is only said. */
	budget_stop: boolean;
}

export interface NetStatus {
	conditions: { metered: boolean; on_battery: boolean; network: string | null };
	usage: { month: string; bytes: number };
	budget: number | null;
	over_budget: boolean;
}

export const netStatus = () => invoke<NetStatus>('net_status');
