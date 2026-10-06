/**
 * Renderer bridge for the `rightkit-shell` Tauri crate.
 *
 * Dependency-free: by default it talks to Tauri through `window.__TAURI_INTERNALS__` (what
 * `@tauri-apps/api` itself uses), so the same file runs under a bundler or as a plain ES module.
 * Pass your own transport to test or to use `@tauri-apps/api` directly.
 *
 * Event names and command names are the crate's stable contract (`rightkit-shell://*`, `rk_shell_*`).
 */
export const EVENTS = {
    menu: 'rightkit-shell://menu',
    tray: 'rightkit-shell://tray',
    deepLink: 'rightkit-shell://deep-link',
    secondInstance: 'rightkit-shell://second-instance',
    stall: 'rightkit-shell://stall',
    engine: 'rightkit-shell://engine',
    window: 'rightkit-shell://window',
};
/** Transport over `window.__TAURI_INTERNALS__`. */
export function tauriTransport() {
    const internals = globalThis.__TAURI_INTERNALS__;
    if (!internals)
        throw new Error('rightkit-shell: not running inside a Tauri webview');
    return {
        invoke: (command, args) => internals.invoke(command, args),
        listen: async (event, handler) => {
            const id = internals.transformCallback((e) => handler(e.payload));
            const eventId = await internals.invoke('plugin:event|listen', { event, target: { kind: 'Any' }, handler: id });
            return () => {
                void internals.invoke('plugin:event|unlisten', { event, eventId });
            };
        },
    };
}
const toBase64 = (b) => {
    let s = '';
    for (let i = 0; i < b.length; i += 0x8000)
        s += String.fromCharCode(...b.subarray(i, i + 0x8000));
    return btoa(s);
};
const fromBase64 = (s) => Uint8Array.from(atob(s), (c) => c.charCodeAt(0));
export function createShell(transport = tauriTransport()) {
    const { invoke, listen } = transport;
    return {
        /** Tell the host the UI is listening: queued deep links are delivered now. Call once after subscribing. */
        ready: () => invoke('rk_shell_ready'),
        status: () => invoke('rk_shell_status'),
        quit: () => invoke('rk_shell_quit'),
        /** Open an http(s)/mailto URL in the system browser. Other schemes are refused by the host. */
        openExternal: (url) => invoke('rk_shell_open_external', { url }),
        window: {
            do: (action) => invoke('rk_shell_window', { action }),
            minimize: () => invoke('rk_shell_window', { action: 'minimize' }),
            toggleMaximize: () => invoke('rk_shell_window', { action: 'toggle-maximize' }),
            close: () => invoke('rk_shell_window', { action: 'close' }),
            startDrag: () => invoke('rk_shell_window', { action: 'start-drag' }),
            open: (spec) => invoke('rk_shell_open_window', { spec }),
            /** macOS: nudge the traffic lights from their default origin (dx right, dy down). */
            trafficLights: (dx, dy) => invoke('rk_shell_traffic_lights', { dx, dy }),
        },
        menu: {
            setEnabled: (id, enabled) => invoke('rk_shell_set_menu_item_enabled', { id, enabled }),
            onClick: (h) => listen(EVENTS.menu, (p) => h(p.id)),
        },
        tray: {
            update: (update) => invoke('rk_shell_set_tray', { update }),
            onEvent: (h) => listen(EVENTS.tray, h),
        },
        deepLinks: {
            on: (h) => listen(EVENTS.deepLink, h),
        },
        onSecondInstance: (h) => listen(EVENTS.secondInstance, h),
        onStall: (h) => listen(EVENTS.stall, h),
        autostart: {
            get: () => invoke('rk_shell_autostart', { enabled: null }),
            set: (enabled) => invoke('rk_shell_autostart', { enabled }),
        },
        clipboard: {
            read: async (board) => {
                const w = await invoke('rk_shell_clipboard_read', { board: board ?? null });
                return {
                    text: w.text ?? undefined,
                    html: w.html ?? undefined,
                    png: w.png ? fromBase64(w.png) : undefined,
                    files: w.files ?? [],
                    custom: Object.fromEntries(Object.entries(w.custom ?? {}).map(([k, v]) => [k, fromBase64(v)])),
                    board,
                };
            },
            write: (c) => invoke('rk_shell_clipboard_write', {
                payload: {
                    text: c.text ?? null,
                    html: c.html ?? null,
                    png: c.png ? toBase64(c.png) : null,
                    files: c.files ?? [],
                    custom: Object.fromEntries(Object.entries(c.custom ?? {}).map(([k, v]) => [k, toBase64(v)])),
                    board: c.board ?? null,
                },
            }),
        },
        engine: {
            /** One engine call over the stdio contract. Events stream through `onEvent` meanwhile. */
            call: (method, params) => invoke('rk_shell_engine_call', { method, params: params ?? null }),
            /** `call === '$lifecycle'` carries `{state:'ready'|'error', ...}` for engine start; `ready()` replays the last one. */
            onEvent: (h) => listen(EVENTS.engine, h),
        },
    };
}
