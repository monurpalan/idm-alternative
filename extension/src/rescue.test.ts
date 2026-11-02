import { test } from 'node:test';
import assert from 'node:assert/strict';

import { fileName, rescuable, rescueMessage, type Interrupted } from './rescue.ts';

const item = (over: Partial<Interrupted> = {}): Interrupted => ({
	id: 1,
	url: 'https://example.com/get?id=7',
	finalUrl: 'https://cdn.example.com/big.iso',
	referrer: 'https://example.com/',
	filename: 'C:\\Users\\u\\Downloads\\big.iso',
	state: 'interrupted',
	error: 'NETWORK_FAILED',
	bytesReceived: 4096,
	totalBytes: 10240,
	...over
});

test('offers downloads cut off by the network or the server, not cancelled ones', () => {
	assert.equal(rescuable(item()), true);
	assert.equal(rescuable(item({ error: 'SERVER_FAILED' })), true);
	assert.equal(rescuable(item({ error: 'USER_CANCELED' })), false);
	assert.equal(rescuable(item({ error: 'USER_SHUTDOWN' })), false);
	assert.equal(rescuable(item({ state: 'complete' })), false);
	assert.equal(rescuable(item({ url: 'blob:https://x/1', finalUrl: '' })), false);
	assert.equal(rescuable(item({ filename: '' })), false);
});

test('tells the app where the browser was saving and how far it got', () => {
	const m = rescueMessage(item(), 'sid=1', 'UA');
	assert.deepEqual(m, {
		type: 'rescue',
		url: 'https://cdn.example.com/big.iso',
		referrer: 'https://example.com/',
		size: 10240,
		target: 'C:\\Users\\u\\Downloads\\big.iso',
		received: 4096,
		cookies: 'sid=1',
		user_agent: 'UA'
	});
	assert.equal(rescueMessage(item({ totalBytes: -1, referrer: '' }), null, 'UA').size, null);
	assert.equal(fileName('C:\\a\\b.iso'), 'b.iso');
	assert.equal(fileName('/home/u/b.iso'), 'b.iso');
});
