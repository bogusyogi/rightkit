use rightkit_app_icon::{cache_key, AppIcons, IconConfig};

fn main() {
    let executable = std::env::current_exe().expect("consumer executable path");
    let path = executable.to_str().expect("UTF-8 executable path");
    let name = executable.file_name().and_then(|s| s.to_str()).unwrap();
    let icons = AppIcons::new(IconConfig::new(None));

    assert_ne!(cache_key(name, Some(path)), cache_key(name, None));
    if let Some(bytes) = icons.png(name, Some(path)) {
        assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert_eq!(icons.png(name, Some(path)).as_ref(), Some(&bytes));
    }
    println!("OK rightkit-app-icon (registry consumer)");
}
