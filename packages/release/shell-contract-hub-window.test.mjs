import assert from "node:assert/strict";
import test from "node:test";
import { hubWindowLabels, scanRust } from "./shell-contract.mjs";

const parsed = (text) => [{ file: "src/lib.rs", text, ...scanRust(text) }];

test("SC-R6: a window built through the shell's hub_window builder creates its WindowBuild label (Pulse's main window)", () => {
  const pulse = `const HUB_LABEL: &str = "main";
fn create_hub_window(app: &tauri::AppHandle) -> Result<tauri::WebviewWindow, String> {
    use rightkit_tauri_shell::hub_window::{build_window_with_native_options, NativeWindowOptions, WindowBuild};
    let spec = WindowBuild {
        title: &config.title,
        inner_size: (config.width, config.height),
        min_size: config.min_width.zip(config.min_height),
        label: HUB_LABEL,
        route: "/",
    };
    build_window_with_native_options(app, &spec, native, |event, detail| { eprintln!("{event}: {detail}"); })
}`;
  assert.deepEqual(hubWindowLabels(parsed(pulse)), { labels: new Set(["main"]), unknown: false });
  assert.deepEqual(hubWindowLabels(parsed(`fn f() { let s = hub_window::WindowBuild { label: "splash", title: "t" }; }`)), { labels: new Set(["splash"]), unknown: false });
});

test("SC-R6: a label that is no literal or string const is unknown, so nothing is concluded; a nested label field is not the window's", () => {
  assert.equal(hubWindowLabels(parsed(`fn f(l: &str) { let s = WindowBuild { label: l, title: "t" }; }`)).unknown, true);
  const nested = hubWindowLabels(parsed(`fn f() { let s = WindowBuild { title: Title { label: "x" }, label: "main" }; }`));
  assert.deepEqual(nested, { labels: new Set(["main"]), unknown: false });
  assert.deepEqual(hubWindowLabels(parsed(`fn f() { let l = "WindowBuild"; }`)), { labels: new Set(), unknown: false });
});
