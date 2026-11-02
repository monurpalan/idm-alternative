// "Why slow?": what holds a running download back, said only when known for certain.

export type Hint =
	/** A speed limit sets the pace: the download's own (`own`), or the one for all. */
	| { kind: 'limit'; rate: number; own: boolean }
	/** The disk cannot write as fast as the data arrives. */
	| { kind: 'disk' }
	/** Connections keep dropping: this many were opened again in the last minute. */
	| { kind: 'drops'; count: number }
	/** The server allows only this many connections. */
	| { kind: 'server'; connections: number }
	/** Downloads go through the proxy set in the settings. */
	| { kind: 'proxy' };
