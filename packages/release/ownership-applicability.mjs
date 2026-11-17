// #994 keeps M22 mandatory for the active desktop apps. Separate migration scopes
// retain their own rulings, including Ember's historical amendment.
export function requiresDesktopAgentSurface(app, profile, data) {
  return profile === "desktop" && (data?.defaultApps ?? []).includes(app);
}
