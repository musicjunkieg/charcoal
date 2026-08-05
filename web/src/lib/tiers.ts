// Shared copy explaining what each threat tier means. Rendered as a legend
// on the dashboard and as tooltips wherever tier badges appear.

export const TIER_DESCRIPTIONS: Record<string, string> = {
	High: 'Hostile engagement with strong topic overlap — review these first',
	Elevated: 'Concerning signals worth a closer look',
	Watch: 'Worth keeping an eye on',
	Low: 'Scored, nothing concerning found'
};
