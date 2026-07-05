// Standalone vitest config for pure-logic tests in src/lib.
//
// Deliberately does NOT use the SvelteKit vite plugin: these tests cover
// extracted TypeScript modules (no components, no $app/$lib aliases), so
// they run in a plain node environment without needing `svelte-kit sync`.
import { defineConfig } from 'vitest/config';

export default defineConfig({
	test: {
		environment: 'node',
		include: ['src/**/*.test.ts']
	}
});
