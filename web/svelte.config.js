import adapterStatic from '@sveltejs/adapter-static';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';

/** @type {import('@sveltejs/kit').Config} */
const config = {
	preprocess: vitePreprocess(),

	kit: {
		// SPA mode: Axum serves index.html for all non-/api/* paths.
		// Client-side routing handles /dashboard, /accounts/*, /login etc.
		adapter: adapterStatic({
			pages: 'build',
			assets: 'build',
			fallback: 'index.html',
			precompress: false,
			strict: false
		}),
		// No prerendering — fully client-side SPA.
		prerender: {
			entries: []
		}
	}
};

export default config;
