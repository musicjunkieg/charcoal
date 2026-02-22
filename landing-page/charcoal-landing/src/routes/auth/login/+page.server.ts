import { redirect } from '@sveltejs/kit';
import type { Actions } from './$types';

// Stub login action for the landing page.
// Replace this with your actual OAuth flow when connecting to the Charcoal app.
export const actions = {
	default: async ({ request }) => {
		const data = await request.formData();
		const handle = data.get('handle')?.toString();

		if (!handle) {
			return { error: 'Please enter your Bluesky handle' };
		}

		// TODO: Replace with actual OAuth redirect to the Charcoal app
		// For now, redirect back to home
		redirect(302, '/');
	}
} satisfies Actions;
