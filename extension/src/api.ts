// The extension API with promises everywhere: Firefox's `browser` namespace guarantees them,
// Chromium's `chrome` has them in Manifest V3 (and has no `browser`).

export const api: typeof chrome =
	(globalThis as unknown as { browser?: typeof chrome }).browser ?? chrome;
