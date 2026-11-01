use super::config_value;

#[test]
fn theme_accepts_auto_dark_light_and_rejects_others() {
    assert_eq!(
        config_value("theme", "auto").unwrap(),
        toml::Value::String(String::new())
    );
    assert_eq!(
        config_value("theme", " Dark ").unwrap(),
        toml::Value::String("dark".into())
    );
    assert_eq!(
        config_value("theme", "light").unwrap(),
        toml::Value::String("light".into())
    );
    assert!(config_value("theme", "solarized").is_err());
}

#[test]
fn vim_accepts_on_off_spellings() {
    for on in ["on", "true", "1", "yes"] {
        assert_eq!(config_value("vim", on).unwrap(), toml::Value::Boolean(true));
    }
    for off in ["off", "false", "0", "no"] {
        assert_eq!(
            config_value("vim", off).unwrap(),
            toml::Value::Boolean(false)
        );
    }
    assert!(config_value("vim", "maybe").is_err());
}

#[test]
fn statusline_validates_items_and_keymap_validates_specs() {
    assert!(config_value("statusline", "cwd, branch").is_ok());
    assert!(config_value("statusline", "bogus").is_err());
    assert!(config_value("keymap.sidebar", "ctrl+b").is_ok());
    assert!(config_value("nope", "x").is_err());
}
