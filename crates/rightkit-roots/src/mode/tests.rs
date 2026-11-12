use super::*;
use rightkit_fs::app_dirs::Os;
use std::ffi::OsString;

/// The installed HeardRight identity: the flat `HeardRightNext` folder.
const HEARD: AppId = AppId::with_dir("heardright", "HeardRightNext").without_org();
const VIEW: AppId = AppId::with_dir("viewright", "ViewRight").with_dev_dir("ViewRightDev");

/// A fake OS: the account's base folders for `os`, the suite under `per_user`, and a set of
/// folders that "exist". Never reads the process environment.
struct Fake {
    os: Os,
    env: Vec<(&'static str, &'static str)>,
    per_user: &'static str,
    existing: Vec<PathBuf>,
}

impl ModeInput for Fake {
    fn env(&self, _: &str) -> Option<String> {
        panic!("roots must not read the environment");
    }
    fn per_user_default(&self) -> Result<Suite, SuiteError> {
        Suite::at(self.per_user)
    }
    fn app_dirs(&self, app: &FolderId) -> Result<AppDirs, AppDirsError> {
        let env = |key: &str| {
            self.env
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| OsString::from(*v))
        };
        AppDirs::resolve_with_legacy(app, &Options::new(self.os).dev(false), &env, &|p| {
            self.existing.iter().any(|e| e == p)
        })
    }
    fn is_dir(&self, path: &Path) -> bool {
        self.existing.iter().any(|e| e == path)
    }
}

fn windows() -> Fake {
    Fake {
        os: Os::Windows,
        env: vec![
            ("APPDATA", r"C:\Users\a\AppData\Roaming"),
            ("LOCALAPPDATA", r"D:\Relocated\Local"),
            ("USERPROFILE", r"C:\Users\a"),
        ],
        per_user: "/unused/Orthic Labs/shared",
        existing: vec![],
    }
}

fn mac() -> Fake {
    Fake {
        os: Os::MacOs,
        env: vec![("HOME", "/Users/a")],
        per_user: "/Users/a/Library/Application Support/Orthic Labs/shared",
        existing: vec![],
    }
}

fn app_dir(app: &AppId, input: &Fake) -> PathBuf {
    roots_with(app, &Mode::Release, input).unwrap().app_dir
}

#[test]
fn heardright_windows_is_local_appdata_flat() {
    // `<Local AppData known folder>\HeardRightNext`: the Local folder (not Roaming), no org.
    let expected = PathBuf::from(r"D:\Relocated\Local").join("HeardRightNext");
    assert_eq!(app_dir(&HEARD, &windows()), expected);
    assert!(!app_dir(&HEARD, &windows())
        .to_string_lossy()
        .contains("Orthic Labs"));
}

#[test]
fn heardright_macos_is_application_support_flat() {
    assert_eq!(
        app_dir(&HEARD, &mac()),
        PathBuf::from("/Users/a/Library/Application Support/HeardRightNext")
    );
}

#[test]
fn heardright_linux_is_local_share_flat() {
    let linux = Fake {
        os: Os::Linux,
        env: vec![("HOME", "/home/a")],
        per_user: "/home/a/.local/share/Orthic Labs/shared",
        existing: vec![],
    };
    assert_eq!(
        app_dir(&HEARD, &linux),
        PathBuf::from("/home/a/.local/share/HeardRightNext")
    );
}

#[test]
fn org_layout_app_is_under_orthic_labs_beside_shared() {
    // Byte-identical to the former `<suite parent>/<dir_name>` rule.
    let input = mac();
    let roots = roots_with(&VIEW, &Mode::Release, &input).unwrap();
    assert_eq!(
        roots.app_dir,
        PathBuf::from("/Users/a/Library/Application Support/Orthic Labs/ViewRight")
    );
    assert_eq!(roots.app_dir.parent(), roots.suite.root().parent());
    let plain = AppId::new("cutright");
    assert_eq!(
        app_dir(&plain, &input),
        PathBuf::from("/Users/a/Library/Application Support/Orthic Labs/cutright")
    );
}

#[test]
fn dev_variant_keeps_layout_and_drops_legacy_names() {
    let dev = VIEW.dev_variant();
    assert_eq!(dev.dir_name(), "ViewRightDev");
    assert_eq!(dev.id(), "viewright");
    assert_eq!(
        app_dir(&dev, &mac()),
        PathBuf::from("/Users/a/Library/Application Support/Orthic Labs/ViewRightDev")
    );
    let flat_dev = HEARD.with_dev_dir("HeardRightNextDev").dev_variant();
    assert_eq!(
        app_dir(&flat_dev, &mac()),
        PathBuf::from("/Users/a/Library/Application Support/HeardRightNextDev")
    );
}

#[test]
fn legacy_folder_is_used_only_when_current_is_missing() {
    let app = AppId::with_dir("app", "New").with_legacy_names(&["Old"]);
    let base = "/Users/a/Library/Application Support/Orthic Labs";
    let mut input = mac();
    assert_eq!(app_dir(&app, &input), PathBuf::from(base).join("New"));
    input.existing = vec![PathBuf::from(base).join("Old")];
    assert_eq!(app_dir(&app, &input), PathBuf::from(base).join("Old"));
    input.existing.push(PathBuf::from(base).join("New"));
    assert_eq!(app_dir(&app, &input), PathBuf::from(base).join("New"));

    let flat = app.without_org();
    let support = "/Users/a/Library/Application Support";
    let mut input = mac();
    input.existing = vec![PathBuf::from(support).join("Old")];
    assert_eq!(app_dir(&flat, &input), PathBuf::from(support).join("Old"));
}

#[test]
fn flat_root_gets_the_same_checks_as_an_org_root() {
    let tmp = std::env::temp_dir().join(format!("rk-roots-mode-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let roots = AppRoots {
        suite: Suite::at(tmp.join("Orthic Labs").join("shared")).unwrap(),
        app_dir: tmp.join("HeardRightNext"),
    };
    let root = crate::AppRoot::for_app(&roots).unwrap();
    assert_eq!(root.name(), "HeardRightNext");
    assert_eq!(root.root(), roots.app_dir);
    assert!(root
        .path("db/x.sqlite")
        .unwrap()
        .starts_with(&roots.app_dir));
    assert!(root.path("../escape").is_err());
    // The case-conflict rule applies in the flat parent too.
    let clash = AppRoots {
        suite: roots.suite.clone(),
        app_dir: tmp.join("heardrightnext"),
    };
    assert!(matches!(
        crate::AppRoot::for_app(&clash),
        Err(crate::AppRootError::CaseConflict { .. })
    ));
    let _ = std::fs::remove_dir_all(&tmp);
}
