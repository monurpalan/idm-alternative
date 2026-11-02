// What an archive, a video or a document holds, read from its skeleton while it downloads.

export type Skeleton =
	| { kind: 'archive'; files: number; unpacked: number; names: string[] }
	/** `duration` in seconds. */
	| { kind: 'video'; duration: number; width: number; height: number }
	| { kind: 'document'; pages: number };
