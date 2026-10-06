//! Tiny standalone Tauri app proving the rightkit-control in-app server.
//! The control plugin is only added when the launcher enables it by env.
use rightkit_control::embedded::Control;

fn main() {
    let mut builder = tauri::Builder::default();
    let control = Control::<tauri::Wry>::new()
        .command("ping", |_app, _body| Ok(r#""pong""#.into()))
        .command("echo", |_app, body| Ok(format!("{{\"echo\":\"{}\"}}", rightkit_control::json::esc(&rightkit_control::json::str_field(body, "args").unwrap_or_default()))));
    if let Some(plugin) = control.build_if_enabled() {
        builder = builder.plugin(plugin);
    }
    builder.run(tauri::generate_context!()).expect("run control-test-app");
}
