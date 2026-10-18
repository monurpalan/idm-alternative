// Puts a `position: fixed` element at a point (the pointer), kept inside the window. The
// point is only known at run time, so this is the one place the UI sets a position itself.
export function placeAt(node: HTMLElement, at: { x: number; y: number }) {
	function place({ x, y }: { x: number; y: number }) {
		const r = node.getBoundingClientRect();
		node.style.left = `${Math.max(0, Math.min(x, window.innerWidth - r.width - 4))}px`;
		node.style.top = `${Math.max(0, Math.min(y, window.innerHeight - r.height - 4))}px`;
	}
	place(at);
	return { update: place };
}
