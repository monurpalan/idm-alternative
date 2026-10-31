// A stand-in for the native messaging host in the extension's end-to-end tests: writes each
// message the extension sends as a line of JSON to $SHARDLOAD_E2E_LOG and answers like the app
// would. A download whose address contains "refuse" is refused, so the extension gives it back
// to the browser.

import { appendFileSync } from 'node:fs';

const log = process.env.SHARDLOAD_E2E_LOG;
let buffer = Buffer.alloc(0);

function reply(message) {
	const body = Buffer.from(JSON.stringify(message));
	const head = Buffer.alloc(4);
	head.writeUInt32LE(body.length);
	process.stdout.write(Buffer.concat([head, body]));
}

function answer(message) {
	if (log) appendFileSync(log, JSON.stringify(message) + '\n');
	if (message.type === 'ping') return { ok: true, version: 'e2e' };
	if (typeof message.url === 'string' && message.url.includes('refuse')) {
		return { ok: false, error: 'refused by the test' };
	}
	return { ok: true };
}

process.stdin.on('data', (chunk) => {
	buffer = Buffer.concat([buffer, chunk]);
	while (buffer.length >= 4) {
		const length = buffer.readUInt32LE(0);
		if (buffer.length < 4 + length) return;
		const message = JSON.parse(buffer.subarray(4, 4 + length).toString('utf8'));
		buffer = buffer.subarray(4 + length);
		reply(answer(message));
	}
});
// The browser closes stdin when it is done with the host (one message for
// `sendNativeMessage`).
process.stdin.on('end', () => process.exit(0));
