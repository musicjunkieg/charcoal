// Full-page screenshots through the Chrome DevTools Protocol, in viewport-high
// slices. Plain `--screenshot` cannot scroll, and its window has a 500px
// minimum width, so it cannot show a phone layout honestly.
//
// Usage (serve the built site first, e.g. npm --prefix web run preview):
//   node web/scripts/page-shots.mjs <url> <out-dir> <name>:<width>:<height>[:still] ...
//   node web/scripts/page-shots.mjs http://localhost:4189/ /tmp/shots d:1440:900 m:390:844 still:1440:900:still
// A trailing :still captures with prefers-reduced-motion set. Each size also
// writes <name>-full.png, the whole page as one image.
import { spawn } from 'node:child_process';
import { writeFileSync, mkdirSync, unlinkSync } from 'node:fs';
import { resolve } from 'node:path';
import { tmpdir } from 'node:os';

const [url, outDir, ...sizes] = process.argv.slice(2);
const PORT = 9333;
mkdirSync(outDir, { recursive: true });

const chrome = spawn(
	'/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
	[
		'--headless=new',
		'--no-sandbox',
		'--disable-gpu',
		'--hide-scrollbars',
		`--remote-debugging-port=${PORT}`,
		// A fresh profile per run, outside the output folder: a reused one is
		// locked by any Chrome that did not exit.
		`--user-data-dir=${tmpdir()}/page-shots-profile-${Date.now()}`,
		'about:blank'
	],
	{ stdio: 'ignore' }
);

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function target() {
	for (let i = 0; i < 60; i++) {
		try {
			const list = await (await fetch(`http://127.0.0.1:${PORT}/json`)).json();
			const page = list.find((t) => t.type === 'page');
			if (page) return page.webSocketDebuggerUrl;
		} catch {
			/* not up yet */
		}
		await sleep(250);
	}
	throw new Error('Chrome never opened its debugging port');
}

try {
	const ws = new WebSocket(await target());
	await new Promise((r) => (ws.onopen = r));
	let id = 0;
	const pending = new Map();
	ws.onmessage = (e) => {
		const m = JSON.parse(e.data);
		if (m.id && pending.has(m.id)) {
			pending.get(m.id)(m.result ?? m);
			pending.delete(m.id);
		}
	};
	const send = (method, params = {}) =>
		new Promise((r) => {
			pending.set(++id, r);
			ws.send(JSON.stringify({ id, method, params }));
		});

	await send('Page.enable');
	for (const size of sizes) {
		const [name, w, h, motion] = size.split(':');
		await send('Emulation.setDeviceMetricsOverride', {
			width: +w,
			height: +h,
			deviceScaleFactor: 1,
			mobile: +w < 600
		});
		await send('Emulation.setEmulatedMedia', {
			features: [{ name: 'prefers-reduced-motion', value: motion === 'still' ? 'reduce' : '' }]
		});
		await send('Page.navigate', { url });
		await sleep(4500); // fonts, then far enough into the loop to see posts mid-journey
		// Scroll to the bottom and back first. Pages that reveal sections as
		// they scroll into view (the old landing did) otherwise capture as blank.
		await send('Runtime.evaluate', {
			awaitPromise: true,
			expression: `(async () => {
				for (let y = 0; y < document.documentElement.scrollHeight; y += innerHeight / 2) {
					scrollTo(0, y);
					await new Promise((r) => setTimeout(r, 120));
				}
				scrollTo(0, 0);
				await new Promise((r) => setTimeout(r, 1200));
			})()`
		});
		const { cssContentSize } = await send('Page.getLayoutMetrics');
		const total = Math.ceil(cssContentSize.height);
		// Each slice is a real scroll position, captured as the visitor sees it.
		// Rendering past the viewport instead (captureBeyondViewport) draws
		// position:fixed backgrounds only once at the top, so every later slice
		// of a page with a fixed backdrop comes out on bare white.
		const slices = [];
		for (let y = 0; y < total; y += +h) {
			const top = Math.max(0, Math.min(y, total - +h));
			await send('Runtime.evaluate', { expression: `scrollTo(0, ${top})` });
			if (slices.length === 1) {
				// A pinned nav would repeat at the top of every slice in the
				// stitched image. Hide fixed elements that hold controls after
				// the first slice; fixed backdrops (no links or buttons) stay.
				await send('Runtime.evaluate', {
					expression: `for (const el of document.querySelectorAll('body *')) {
						if (getComputedStyle(el).position === 'fixed' && el.querySelector('a, button')) el.style.visibility = 'hidden';
					}`
				});
			}
			await sleep(900); // let scroll-triggered reveals finish
			const { data } = await send('Page.captureScreenshot', { format: 'png' });
			const file = `${name}-${String(slices.length).padStart(2, '0')}.png`;
			writeFileSync(`${outDir}/${file}`, Buffer.from(data, 'base64'));
			// The last slice is clamped to the page end and overlaps the one
			// before; `skip` is how much of its top the stitched image drops.
			slices.push({ file, skip: y - top, height: Math.min(+h, total - y) });
		}
		// One tall image for sharing, stitched in the browser from the slices.
		const stitch = `${outDir}/.${name}-stitch.html`;
		writeFileSync(
			stitch,
			`<!doctype html><body style="margin:0">${slices
				.map(
					(s) =>
						`<div style="height:${s.height}px;overflow:hidden"><img src="${s.file}" style="display:block;margin-top:-${s.skip}px"></div>`
				)
				.join('')}</body>`
		);
		await send('Page.navigate', { url: `file://${resolve(stitch)}` });
		await sleep(1500);
		const { data: full } = await send('Page.captureScreenshot', {
			format: 'png',
			captureBeyondViewport: true,
			clip: { x: 0, y: 0, width: +w, height: total, scale: 1 }
		});
		writeFileSync(`${outDir}/${name}-full.png`, Buffer.from(full, 'base64'));
		unlinkSync(stitch);
		console.log(`${name}: ${w}x${total} content (width ${Math.ceil(cssContentSize.width)}), ${slices.length} slices`);
	}
	ws.close();
} finally {
	chrome.kill('SIGKILL');
}
