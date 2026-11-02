// Reading the PNGs the DevTools Protocol takes (8 bits per channel, RGB or RGBA, not
// interlaced) and comparing two of them with a tolerance, without dependencies.

import { inflateSync } from 'node:zlib';

/** `{ width, height, rgba }` of a PNG. */
export function decode(png) {
	let at = 8;
	let width = 0;
	let height = 0;
	let channels = 0;
	const data = [];
	while (at < png.length) {
		const length = png.readUInt32BE(at);
		const type = png.toString('ascii', at + 4, at + 8);
		const body = png.subarray(at + 8, at + 8 + length);
		if (type === 'IHDR') {
			width = body.readUInt32BE(0);
			height = body.readUInt32BE(4);
			const [depth, colour, , , interlace] = body.subarray(8, 13);
			channels = { 2: 3, 6: 4 }[colour];
			if (depth !== 8 || !channels || interlace) throw new Error('unsupported PNG');
		} else if (type === 'IDAT') {
			data.push(body);
		}
		at += 12 + length;
	}
	const raw = inflateSync(Buffer.concat(data));
	const stride = width * channels;
	const out = Buffer.alloc(width * height * channels);
	for (let y = 0; y < height; y++) {
		const filter = raw[y * (stride + 1)];
		const line = raw.subarray(y * (stride + 1) + 1, (y + 1) * (stride + 1));
		for (let x = 0; x < stride; x++) {
			const left = x >= channels ? out[y * stride + x - channels] : 0;
			const up = y > 0 ? out[(y - 1) * stride + x] : 0;
			const corner = x >= channels && y > 0 ? out[(y - 1) * stride + x - channels] : 0;
			out[y * stride + x] = (line[x] + predict(filter, left, up, corner)) & 0xff;
		}
	}
	const rgba = Buffer.alloc(width * height * 4, 0xff);
	for (let i = 0; i < width * height; i++) out.copy(rgba, i * 4, i * channels, i * channels + Math.min(channels, 4));
	return { width, height, rgba };
}

function predict(filter, a, b, c) {
	switch (filter) {
		case 0:
			return 0;
		case 1:
			return a;
		case 2:
			return b;
		case 3:
			return (a + b) >> 1;
		case 4: {
			const p = a + b - c;
			const [pa, pb, pc] = [Math.abs(p - a), Math.abs(p - b), Math.abs(p - c)];
			return pa <= pb && pa <= pc ? a : pb <= pc ? b : c;
		}
		default:
			throw new Error(`PNG filter ${filter}`);
	}
}

/**
 * The share of pixels that differ by more than `tolerance` in any channel (1 when the sizes
 * differ): small differences in anti-aliasing pass, a moved or recoloured element does not.
 */
export function difference(a, b, tolerance = 24) {
	if (a.width !== b.width || a.height !== b.height) return 1;
	let differing = 0;
	for (let i = 0; i < a.rgba.length; i += 4) {
		for (let c = 0; c < 3; c++) {
			if (Math.abs(a.rgba[i + c] - b.rgba[i + c]) > tolerance) {
				differing++;
				break;
			}
		}
	}
	return differing / (a.width * a.height);
}
