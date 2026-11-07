import { test } from 'node:test';
import assert from 'node:assert/strict';
import { GAP, MARGIN, placeButton } from './place.ts';

const button = { width: 100, height: 24 };
const view = { width: 1280, height: 800 };

test('sits just above the top right corner, outside the video', () => {
	const p = placeButton({ top: 100, right: 900, bottom: 500, left: 100 }, button, view);
	assert.deepEqual(p, { top: 100 - 24 - GAP, left: 800, inside: false });
});

test('goes below the video when there is no room above', () => {
	const p = placeButton({ top: 10, right: 900, bottom: 500, left: 100 }, button, view);
	assert.deepEqual(p, { top: 500 + GAP, left: 800, inside: false });
});

test('goes inside only when the video fills the window', () => {
	const p = placeButton({ top: 0, right: 1280, bottom: 800, left: 0 }, button, view);
	assert.equal(p.inside, true);
	assert.ok(p.top >= MARGIN && p.left + button.width <= view.width - MARGIN);
});

test('stays in the window when the video runs off its side', () => {
	const p = placeButton({ top: 300, right: 1600, bottom: 700, left: 800 }, button, view);
	assert.equal(p.left, view.width - button.width - MARGIN);
	const q = placeButton({ top: 300, right: 50, bottom: 700, left: -400 }, button, view);
	assert.equal(q.left, MARGIN);
});
