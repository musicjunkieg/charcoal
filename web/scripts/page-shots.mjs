// Full-page screenshots through the Chrome DevTools Protocol, in viewport-high
// slices. Plain `--screenshot` cannot scroll, and its window has a 500px
// minimum width, so it cannot show a phone layout honestly.
//
// Usage (serve the built site first, e.g. npm --prefix web run preview):
//   node web/scripts/page-shots.mjs <url> <out-dir> <name>:<width>:<height>[:still] ...
//   node web/scripts/page-shots.mjs http://localhost:4189/ /tmp/shots d:1440:900 m:390:844 still:1440:900:still
// A trailing :still captures with prefers-reduced-motion set.
import { spawn } from 'node:child_process';
import { writeFileSync, mkdirSync } from 'node:fs';

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
		`--user-data-dir=${outDir}/cdp-profile-${Date.now()}`,
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
		const { cssContentSize } = await send('Page.getLayoutMetrics');
		const total = Math.ceil(cssContentSize.height);
		let n = 0;
		for (let y = 0; y < total; y += +h) {
			const { data } = await send('Page.captureScreenshot', {
				format: 'png',
				captureBeyondViewport: true,
				clip: { x: 0, y, width: +w, height: Math.min(+h, total - y), scale: 1 }
			});
			writeFileSync(`${outDir}/${name}-${String(n++).padStart(2, '0')}.png`, Buffer.from(data, 'base64'));
		}
		console.log(`${name}: ${w}x${total} content (width ${Math.ceil(cssContentSize.width)}), ${n} slices`);
	}
	ws.close();
} finally {
	chrome.kill('SIGKILL');
}
