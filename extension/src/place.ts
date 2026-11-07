// Where the button over a video goes: just outside the video's top right corner, like IDM's
// panel, so it never covers the picture or the player's own controls. Below the video when
// there is no room above it, and inside its top right corner only when there is no room
// outside at all (a video filling the window). Pure, tested with node:test.

export interface Rect {
	top: number;
	right: number;
	bottom: number;
	left: number;
}

/** Space between the video and the button. */
export const GAP = 2;
/** Space kept from the window's edges. */
export const MARGIN = 4;

export function placeButton(
	video: Rect,
	button: { width: number; height: number },
	view: { width: number; height: number }
): { top: number; left: number; inside: boolean } {
	const left = Math.min(Math.max(MARGIN, video.right - button.width), view.width - button.width - MARGIN);
	const above = video.top - button.height - GAP;
	if (above >= MARGIN) return { top: above, left, inside: false };
	const below = video.bottom + GAP;
	if (below + button.height <= view.height - MARGIN) return { top: below, left, inside: false };
	return { top: Math.max(MARGIN, video.top + 8), left: Math.max(MARGIN, left - 8), inside: true };
}
