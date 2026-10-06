//! A simulated desktop implementing `Platform`, for exercising the engine
//! (ids, freshness guard, ladder, safety, delivery) on every host and for
//! product tests that must not touch a real desktop.

use crate::model::*;
use crate::platform::*;
use crate::proof::{FieldText, TextSource};
use std::path::Path;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug)]
pub struct MockEl {
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub raw: RawNode,
    pub pid: i32,
    pub window: u64,
    /// Editable text (None = not a text element).
    pub text: Option<String>,
    pub sel: (usize, usize),
    /// Text cannot be read back (blind proof).
    pub blind: bool,
    pub alive: bool,
}

#[derive(Debug)]
pub struct MockState {
    pub trusted: bool,
    pub apps: Vec<AppInfo>,
    pub front: i32,
    pub els: Vec<MockEl>,
    pub focused: Option<usize>,
    /// Capability and behaviour toggles.
    pub semantic_ok: bool,
    pub native_write_capable: bool,
    /// Acknowledge native writes without applying them.
    pub native_write_lies: bool,
    pub post_lands: bool,
    /// Posted events land as a different edit (not attributable).
    pub post_garbles: bool,
    /// Targeted posting fails outright (window ignores addressed events).
    pub post_fails: bool,
    pub post_steals_focus: bool,
    pub foreground_lands: bool,
    pub clip: Option<String>,
    pub clip_change: u64,
    pub clip_lossless: bool,
    pub pressed: Vec<usize>,
    /// Ordered record of platform calls for assertions.
    pub log: Vec<String>,
    pub next_pid: i32,
}

#[derive(Clone)]
pub struct MockPlatform(pub Arc<Mutex<MockState>>);

pub const EDITOR_PID: i32 = 100;
pub const OTHER_PID: i32 = 200;

impl MockPlatform {
    /// Element indices of the standard fixture.
    pub const ROOT: usize = 0;
    pub const SAVE: usize = 1;
    pub const DELETE: usize = 2;
    pub const AREA: usize = 3;
    pub const PASSWORD: usize = 4;

    /// An editor app (pid 100) with a window, buttons, a text area and a
    /// password field, plus an unrelated frontmost app (pid 200).
    pub fn fixture() -> Self {
        let app = |pid, id: &str| AppInfo { pid, app_id: id.into(), name: id.into(), path: None, started: Some(pid as u64 * 1000) };
        let mk = |parent: Option<usize>, role: &str, name: Option<&str>, caps: &[&str]| MockEl {
            parent,
            children: vec![],
            raw: RawNode {
                role: role.into(),
                name: name.map(str::to_string),
                enabled: Some(true),
                frame: Some(Rect { x: 10.0, y: 10.0, w: 80.0, h: 20.0 }),
                caps: caps.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            },
            pid: EDITOR_PID,
            window: 7,
            text: None,
            sel: (0, 0),
            blind: false,
            alive: true,
        };
        let mut els = vec![
            mk(None, "window", Some("Doc"), &[]),
            mk(Some(0), "button", Some("Save"), &["press"]),
            mk(Some(0), "button", Some("Delete"), &["press"]),
            mk(Some(0), "textarea", Some("Body"), &["set_value", "insert_text", "focus"]),
            mk(Some(0), "textfield", Some("Password"), &["set_value"]),
        ];
        els[0].children = vec![1, 2, 3, 4];
        els[3].text = Some(String::new());
        els[4].text = Some(String::new());
        els[4].raw.secure = true;
        MockPlatform(Arc::new(Mutex::new(MockState {
            trusted: true,
            apps: vec![app(EDITOR_PID, "com.test.editor"), app(OTHER_PID, "com.test.other")],
            front: OTHER_PID,
            els,
            focused: Some(Self::AREA),
            semantic_ok: true,
            native_write_capable: true,
            native_write_lies: false,
            post_lands: true,
            post_garbles: false,
            post_fails: false,
            post_steals_focus: false,
            foreground_lands: true,
            clip: Some("user clipboard".into()),
            clip_change: 1,
            clip_lossless: true,
            pressed: vec![],
            log: vec![],
            next_pid: 300,
        })))
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut MockState) -> R) -> R {
        f(&mut self.0.lock().unwrap())
    }
    pub fn log(&self) -> Vec<String> {
        self.with(|s| s.log.clone())
    }
    pub fn area_text(&self) -> String {
        self.with(|s| s.els[Self::AREA].text.clone().unwrap_or_default())
    }
    pub fn set_area(&self, text: &str, sel: (usize, usize)) {
        self.with(|s| {
            s.els[Self::AREA].text = Some(text.into());
            s.els[Self::AREA].sel = sel;
        });
    }
    /// Add a button to the window after the fact (a structural change).
    pub fn add_button(&self, name: &str) -> usize {
        self.with(|s| {
            let mut e = s.els[Self::SAVE].clone();
            e.raw.name = Some(name.into());
            e.parent = Some(0);
            s.els.push(e);
            let i = s.els.len() - 1;
            s.els[0].children.push(i);
            i
        })
    }
}

fn insert(el: &mut MockEl, text: &str) {
    let t = el.text.clone().unwrap_or_default();
    let units: Vec<u16> = t.encode_utf16().collect();
    let (loc, len) = el.sel;
    let end = (loc + len).min(units.len());
    let loc = loc.min(units.len());
    let head = String::from_utf16_lossy(&units[..loc]);
    let tail = String::from_utf16_lossy(&units[end..]);
    el.text = Some(format!("{head}{text}{tail}"));
    el.sel = (loc + text.encode_utf16().count(), 0);
}

impl MockState {
    fn apply_events(&mut self, pid: i32, ev: &InputEvent, foreground: bool) -> Result<()> {
        let lands = if foreground { self.foreground_lands } else { self.post_lands };
        let Some(f) = self.focused.filter(|i| self.els[*i].pid == pid) else { return Ok(()) };
        match ev {
            InputEvent::Text(t) if lands => {
                let t = if self.post_garbles && !foreground { format!("#{t}#").replace(t.as_str(), "?") } else { t.clone() };
                insert(&mut self.els[f], &t)
            }
            InputEvent::Key(c) => {
                use crate::keys::{Key, Named};
                if lands && c.key == Key::Char('a') && (c.mods.meta || c.mods.ctrl) {
                    let n = self.els[f].text.clone().unwrap_or_default().encode_utf16().count();
                    self.els[f].sel = (0, n);
                } else if lands && c.key == Key::Char('v') && (c.mods.meta || c.mods.ctrl) {
                    if let Some(clip) = self.clip.clone() {
                        insert(&mut self.els[f], &clip);
                    }
                } else if lands && c.key == Key::Named(Named::Return) {
                    insert(&mut self.els[f], "\n");
                }
            }
            _ => {}
        }
        Ok(())
    }
}

impl Platform for MockPlatform {
    type El = usize;

    fn kind(&self) -> PlatformKind {
        PlatformKind::Mock
    }
    fn status(&self) -> Status {
        let t = self.with(|s| s.trusted);
        Status { platform: PlatformKind::Mock, accessibility: t, screen_capture: true, notes: vec![] }
    }
    fn apps(&self) -> Vec<AppInfo> {
        self.with(|s| s.apps.clone())
    }
    fn windows(&self, _on_screen_only: bool) -> Vec<WindowInfo> {
        vec![WindowInfo {
            window_id: 7,
            pid: EDITOR_PID,
            owner: "com.test.editor".into(),
            title: "Doc".into(),
            layer: 0,
            on_screen: true,
            bounds: Rect { x: 0.0, y: 0.0, w: 400.0, h: 300.0 },
        }]
    }
    fn frontmost(&self) -> Option<FrontApp> {
        self.with(|s| {
            let a = s.apps.iter().find(|a| a.pid == s.front)?;
            Some(FrontApp { pid: a.pid, app_id: a.app_id.clone(), window_id: None })
        })
    }
    fn root(&self, t: &Target) -> Result<usize> {
        self.with(|_s| if t.pid == EDITOR_PID { Ok(MockPlatform::ROOT) } else { Err(Error::NotFound(format!("pid {}", t.pid))) })
    }
    fn read(&self, el: &usize) -> Option<RawNode> {
        self.with(|s| {
            let e = s.els.get(*el).filter(|e| e.alive)?;
            let mut r = e.raw.clone();
            if e.text.is_some() && !e.blind && !r.secure {
                r.value = e.text.clone();
            }
            Some(r)
        })
    }
    fn children(&self, el: &usize) -> Vec<usize> {
        self.with(|s| s.els.get(*el).map(|e| e.children.clone()).unwrap_or_default())
    }
    fn focused(&self, pid: i32) -> Result<Option<usize>> {
        Ok(self.with(|s| s.focused.filter(|i| s.els[*i].pid == pid && s.els[*i].alive)))
    }
    fn same(&self, a: &usize, b: &usize) -> bool {
        a == b
    }
    fn window_of(&self, el: &usize) -> Option<u64> {
        self.with(|s| s.els.get(*el).map(|e| e.window))
    }
    fn capture(&self, _w: u64, path: &Path) -> Result<()> {
        let png = crate::png::encode_rgba(400, 300, &vec![255u8; 400 * 300 * 4]).unwrap();
        std::fs::write(path, png).map_err(|_| Error::Platform(0, "write png"))
    }
    fn semantic(&self, el: &usize, action: &Action) -> Result<String> {
        self.with(|s| {
            if !s.semantic_ok {
                return Err(Error::Unsupported("semantic disabled"));
            }
            let caps = s.els[*el].raw.caps.clone();
            let need = match action {
                Action::Press => "press",
                Action::SetValue(_) => "set_value",
                Action::Focus => "focus",
                Action::Scroll { .. } => "scroll",
                _ => return Err(Error::Unsupported("not semantic")),
            };
            if !caps.iter().any(|c| c == need) {
                return Err(Error::Unsupported("element lacks capability"));
            }
            s.log.push(format!("semantic:{}:{el}", action.name()));
            match action {
                Action::Press => s.pressed.push(*el),
                Action::SetValue(v) => {
                    s.els[*el].text = Some(v.clone());
                    s.els[*el].sel = (v.encode_utf16().count(), 0);
                }
                Action::Focus => s.focused = Some(*el),
                _ => {}
            }
            Ok(format!("mock {need}"))
        })
    }
    fn menu_press(&self, pid: i32, path: &[String]) -> Result<()> {
        self.with(|s| {
            s.log.push(format!("menu:{pid}:{}", path.join(">")));
            Ok(())
        })
    }
    fn field_text(&self, el: &usize) -> Option<FieldText> {
        self.with(|s| {
            let e = s.els.get(*el).filter(|e| e.alive && !e.blind)?;
            Some(FieldText { text: e.text.clone()?, source: Some(TextSource::Value), selection: Some(e.sel) })
        })
    }
    fn native_write_capable(&self, el: &usize) -> bool {
        self.with(|s| s.native_write_capable && s.els.get(*el).is_some_and(|e| e.text.is_some() && !e.raw.secure && !e.blind))
    }
    fn native_write(&self, el: &usize, text: &str) -> Result<()> {
        self.with(|s| {
            s.log.push(format!("native_write:{el}"));
            if !s.native_write_lies {
                insert(&mut s.els[*el], text);
            }
            Ok(())
        })
    }
    fn post(&self, pid: i32, _w: Option<u64>, ev: &InputEvent) -> Result<()> {
        self.with(|s| {
            s.log.push(format!("post:{pid}:{}", short(ev)));
            if s.post_fails {
                return Err(Error::Platform(-1, "post refused"));
            }
            if s.post_steals_focus {
                s.front = pid;
            }
            s.apply_events(pid, ev, false)
        })
    }
    fn activate(&self, pid: i32) -> bool {
        self.with(|s| {
            s.log.push(format!("activate:{pid}"));
            s.front = pid;
            true
        })
    }
    fn foreground(&self, pid: i32, ev: &InputEvent) -> Result<()> {
        self.with(|s| {
            s.log.push(format!("foreground:{pid}:{}", short(ev)));
            s.apply_events(pid, ev, true)
        })
    }
    fn clip_snapshot(&self) -> Option<ClipSnapshot> {
        self.with(|s| Some(ClipSnapshot { text: s.clip.clone(), lossless: s.clip_lossless, change: s.clip_change, formats: 1 }))
    }
    fn clip_write(&self, text: &str) -> Option<u64> {
        self.with(|s| {
            s.clip = Some(text.into());
            s.clip_change += 1;
            s.log.push("clip_write".into());
            Some(s.clip_change)
        })
    }
    fn clip_restore(&self, snap: &ClipSnapshot, expected: u64) -> bool {
        self.with(|s| {
            if s.clip_change != expected {
                return false;
            }
            s.clip = snap.text.clone();
            s.clip_change += 1;
            s.log.push("clip_restore".into());
            true
        })
    }
    fn clip_change(&self) -> u64 {
        self.with(|s| s.clip_change)
    }
    fn launch(&self, spec: &str) -> Result<i32> {
        self.with(|s| {
            let pid = s.next_pid;
            s.next_pid += 1;
            s.apps.push(AppInfo { pid, app_id: spec.into(), name: spec.into(), path: None, started: Some(pid as u64) });
            s.log.push(format!("launch:{spec}"));
            Ok(pid)
        })
    }
    fn terminate(&self, pid: i32) -> Result<()> {
        self.with(|s| {
            s.apps.retain(|a| a.pid != pid);
            s.log.push(format!("terminate:{pid}"));
            Ok(())
        })
    }
}

fn short(ev: &InputEvent) -> String {
    match ev {
        InputEvent::Text(t) => format!("text({t})"),
        InputEvent::Key(c) => format!("key({:?})", c.key),
        InputEvent::Click { x, y, .. } => format!("click({x},{y})"),
        InputEvent::Scroll { dy, .. } => format!("scroll({dy})"),
    }
}
