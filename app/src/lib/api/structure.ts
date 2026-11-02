// A finished file without a checksum, checked by its own structure.

export interface Structure {
	/** `ZIP`, `gzip`, `xz`, `PNG`, `PDF`, `MP4`, `Matroska`. */
	format: string;
	/** Bytes that were damaged and downloaded again. */
	repaired: number;
	/** Still damaged after that: the file on the server is. */
	damaged: boolean;
}
