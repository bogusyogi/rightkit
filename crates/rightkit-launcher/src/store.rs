//! Store (packaged) apps for the launcher. They have no shortcut in the Start menu's
//! folders; Windows lists them in the shell's Apps folder, where each has a name and an app
//! user model id ("Microsoft.WindowsCalculator_8wekyb3d8bbwe!App") that the shell can open
//! as `shell:AppsFolder\<id>`.

use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree};
use windows::Win32::UI::Shell::{
    BHID_EnumItems, FOLDERID_AppsFolder, IEnumShellItems, IShellItem, KF_FLAG_DEFAULT,
    SHGetKnownFolderItem, SIGDN, SIGDN_NORMALDISPLAY, SIGDN_PARENTRELATIVEPARSING,
};

/// What opens a packaged app: the shell's Apps folder and the app's id.
pub const PREFIX: &str = "shell:AppsFolder\\";

/// Most apps a PC can list here; more than this is not a Start menu.
const MOST: usize = 2000;

fn name(item: &IShellItem, form: SIGDN) -> Option<String> {
    // SAFETY: the shell returns a NUL-terminated string it allocated; it is copied and then
    // freed with the allocator the shell used.
    unsafe {
        let text = item.GetDisplayName(form).ok()?;
        let copied = text.to_string().ok();
        CoTaskMemFree(Some(text.0 as *const _));
        copied
    }
}

/// Each packaged app's name and what opens it. Empty when the shell cannot list them.
pub fn apps() -> Vec<(String, String)> {
    let mut found = Vec::new();
    // SAFETY: COM calls on this thread, which stays in its apartment for its whole life (a
    // second initialisation only adds a count). Every interface is released when dropped.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let Ok(folder) =
            SHGetKnownFolderItem::<IShellItem>(&FOLDERID_AppsFolder, KF_FLAG_DEFAULT, None)
        else {
            return found;
        };
        let Ok(items) = folder.BindToHandler::<_, IEnumShellItems>(None, &BHID_EnumItems) else {
            return found;
        };
        for _ in 0..MOST {
            let mut slot = [None];
            let mut fetched = 0u32;
            if items.Next(&mut slot, Some(&mut fetched)).is_err() || fetched == 0 {
                break;
            }
            let Some(item) = slot[0].take() else {
                break;
            };
            let (Some(title), Some(id)) = (
                name(&item, SIGDN_NORMALDISPLAY),
                name(&item, SIGDN_PARENTRELATIVEPARSING),
            ) else {
                continue;
            };
            // A package's id is "Family_publisher!App"; other entries are desktop programs
            // the Start menu's shortcuts already give.
            if id.contains('!') && id.contains('_') && !title.is_empty() {
                found.push((title, format!("{PREFIX}{id}")));
            }
        }
    }
    found
}
