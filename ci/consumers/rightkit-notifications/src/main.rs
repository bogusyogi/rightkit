// rightkit-notifications 0.1.0 consumed by exact registry pin (`=0.1.0`), outside the workspace.
// Nothing is shown, replaced, withdrawn or badged, no permission prompt is raised and no activation
// handler is registered: `send`, `close`, `close_all`, `delivered`, `set_badge`, `request_permission`
// and `on_activation` act on the person's notification state and are never called. This consumer builds
// notifications, reads the platform capability table and queries the current permission, which never
// prompts.
use rightkit_notifications::{
    Action, Activation, ActivationKind, Capabilities, Client, Error, Notification, Permission,
    Sound,
};

fn check(name: &str, ok: bool) {
    if !ok {
        eprintln!("FAIL: {name}");
        std::process::exit(1);
    }
    println!("ok: {name}");
}

// The documented capability table, per platform.
fn expected_capabilities() -> Capabilities {
    #[cfg(target_os = "macos")]
    {
        Capabilities {
            actions: true,
            activation: true,
            replace: true,
            close: true,
            permission: true,
            badge: true,
            delivered: true,
        }
    }
    #[cfg(windows)]
    {
        Capabilities {
            actions: false,
            activation: false,
            replace: true,
            close: true,
            permission: true,
            badge: true,
            delivered: false,
        }
    }
    #[cfg(target_os = "linux")]
    {
        Capabilities {
            actions: false,
            activation: false,
            replace: true,
            close: true,
            permission: false,
            badge: false,
            delivered: false,
        }
    }
    #[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
    {
        Capabilities {
            actions: false,
            activation: false,
            replace: false,
            close: false,
            permission: false,
            badge: false,
            delivered: false,
        }
    }
}

fn main() {
    // The builder.
    let plain = Notification::new("Build finished");
    check(
        "defaults",
        plain.title == "Build finished"
            && plain.body.is_empty()
            && plain.key.is_none()
            && plain.thread.is_none()
            && plain.sound == Sound::Silent
            && plain.actions.is_empty(),
    );
    let rich = Notification::new("Build finished")
        .body("3 warnings")
        .key("build")
        .thread("ci")
        .sound(Sound::Named("Glass".to_string()))
        .action("open", "Open")
        .action("dismiss", "Dismiss");
    check(
        "chained setters",
        rich.body == "3 warnings"
            && rich.key.as_deref() == Some("build")
            && rich.thread.as_deref() == Some("ci")
            && rich.sound == Sound::Named("Glass".to_string())
            && rich.actions
                == vec![
                    Action {
                        id: "open".to_string(),
                        title: "Open".to_string(),
                    },
                    Action {
                        id: "dismiss".to_string(),
                        title: "Dismiss".to_string(),
                    },
                ],
    );
    check(
        "a changed notification differs",
        rich != plain && rich.clone() == rich,
    );
    check(
        "default sound is silent",
        Sound::default() == Sound::Silent && Sound::Default != Sound::Silent,
    );

    // Activation values.
    let click = Activation {
        key: "build".to_string(),
        kind: ActivationKind::Action("open".to_string()),
    };
    check(
        "activation kinds",
        click.kind == ActivationKind::Action("open".to_string())
            && ActivationKind::Default != ActivationKind::Dismiss,
    );

    // The client: the app id is trimmed.
    let client = Client::new("  com.example.consumer  ");
    check(
        "app id is trimmed",
        client.app_id() == "com.example.consumer",
    );
    check(
        "a clone keeps the app id",
        client.clone().app_id() == "com.example.consumer",
    );

    // The capability table is fixed per platform.
    check(
        "capabilities match the documented table",
        client.capabilities() == expected_capabilities(),
    );

    // Permission is a read: it never prompts. Hosts without a bundle or an AUMID may answer an error.
    match client.permission() {
        Ok(permission) => {
            println!("ok: permission is {permission:?}");
            check(
                "permission is one of the documented answers",
                matches!(
                    permission,
                    Permission::Granted
                        | Permission::Denied
                        | Permission::NotDetermined
                        | Permission::Provisional
                        | Permission::Unsupported
                ),
            );
        }
        Err(error) => println!("ok: permission unavailable on this host: {error}"),
    }

    // Error text.
    check(
        "error text",
        Error::Unsupported("no bundle".to_string()).to_string() == "unsupported: no bundle"
            && Error::PermissionDenied("off".to_string()).to_string() == "permission denied: off"
            && Error::Timeout("slow".to_string()).to_string() == "timeout: slow"
            && Error::Backend("boom".to_string()).to_string() == "boom",
    );

    println!("OK rightkit-notifications 0.1.0 (registry consumer, public API)");
}
