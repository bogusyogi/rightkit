//! rightkit-control CLI (macOS). Every command targets a pid and never
//! activates the app.
#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("rightkit-control: this build supports macOS only (Windows backend: see design.md)");
    std::process::exit(2);
}

#[cfg(target_os = "macos")]
fn main() {
    use rightkit_control::{keys, mac, webdriver};
    let a: Vec<String> = std::env::args().skip(1).collect();
    let arg = |i: usize| a.get(i).map(String::as_str).unwrap_or("");
    let num = |i: usize| arg(i).parse::<f64>().unwrap_or(0.0);
    let pid = || arg(1).parse::<i32>().unwrap_or(0);
    let win = |p: i32| {
        mac::main_window(p).unwrap_or_else(|| {
            eprintln!("no window for pid {p}");
            std::process::exit(1)
        })
    };
    match arg(0) {
        "trusted" => println!("{}", mac::accessibility_trusted()),
        "frontmost" => println!("{} {}", mac::frontmost_pid().unwrap_or(0), mac::frontmost_name().unwrap_or_default()),
        "launch" => {
            // launch <App.app> <exe-name> [K=V ...]
            let env: Vec<(String, String)> = a[3.min(a.len())..].iter().filter_map(|kv| kv.split_once('=')).map(|(k, v)| (k.into(), v.into())).collect();
            match mac::launch_hidden(arg(1), arg(2), &env) {
                Ok(p) => println!("{p}"),
                Err(e) => { eprintln!("{e}"); std::process::exit(1) }
            }
        }
        "windows" => for w in mac::windows(pid()) {
            println!("id={} layer={} onscreen={} {:.0},{:.0} {:.0}x{:.0} {:?}", w.id, w.layer, w.onscreen, w.x, w.y, w.w, w.h, w.name);
        },
        "ax" => for l in mac::ax_dump(pid(), arg(2).parse().unwrap_or(12), 20_000) { println!("{l}") },
        "find" => match mac::ax_find(pid(), arg(2), false) {
            Some(n) => println!("{} {:?} {:?}", n.role, n.text, n.frame),
            None => { eprintln!("not found"); std::process::exit(1) }
        },
        "click" => { let p = pid(); mac::click(p, win(p).id, num(2), num(3)) }
        "click-text" => {
            let p = pid();
            match mac::ax_find(p, arg(2), false) {
                Some(n) => if let Err(e) = mac::click_node(p, &n) { eprintln!("{e}"); std::process::exit(1) },
                None => { eprintln!("not found"); std::process::exit(1) }
            }
        }
        "press" => match mac::ax_find(pid(), arg(2), false) {
            Some(n) => println!("{}", mac::ax_press(&n)),
            None => { eprintln!("not found"); std::process::exit(1) }
        },
        "keywin" => println!("{:?}", mac::ax_make_key(pid(), &a[2..].iter().map(String::as_str).collect::<Vec<_>>())),
        "keyraw" => { let p = pid(); println!("{}", mac::key_without_raise(p, win(p).id)) }
        "hover" => { let p = pid(); mac::hover(p, win(p).id, num(2), num(3)) }
        "drag" => { let p = pid(); mac::drag(p, win(p).id, (num(2), num(3)), (num(4), num(5)), 20) }
        "scroll" => { let p = pid(); mac::scroll(p, win(p).id, num(2), num(3), num(4) as i32) }
        "type" => mac::type_text(pid(), arg(2)),
        "key" => match keys::chord(arg(2)) {
            Some((k, f)) => mac::key(pid(), k, f),
            None => { eprintln!("unknown key {}", arg(2)); std::process::exit(1) }
        },
        "shot" => { let p = pid(); if !mac::capture_window(win(p).id, arg(2)) { std::process::exit(1) } }
        "serve" => {
            // serve <pid> <port|0> <credential-file>
            // The per-launch credential is generated here, written owner-only to the
            // file (never printed), and required as `Authorization: Bearer` on every request.
            use std::io::{Read, Write};
            use std::os::unix::fs::OpenOptionsExt;
            let port: u16 = arg(2).parse().unwrap_or(0);
            let cred_path = arg(3);
            if cred_path.is_empty() {
                eprintln!("usage: rightkit-control serve <pid> <port|0> <credential-file>");
                std::process::exit(2);
            }
            let mut raw = [0u8; 32];
            if std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut raw)).is_err() {
                eprintln!("no system randomness");
                std::process::exit(1);
            }
            let cred: String = raw.iter().map(|b| format!("{b:02x}")).collect();
            let wrote = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(cred_path).and_then(|mut f| f.write_all(cred.as_bytes()));
            if let Err(e) = wrote {
                eprintln!("credential file {cred_path}: {e}");
                std::process::exit(1);
            }
            let listener = match std::net::TcpListener::bind(("127.0.0.1", port)) {
                Ok(l) => l,
                Err(e) => { eprintln!("{e}"); std::process::exit(1) }
            };
            let bound = listener.local_addr().map(|a| a.port()).unwrap_or(0);
            let mut s = webdriver::Server::new(webdriver::macos::AxBackend::new(pid()));
            eprintln!("rightkit-control WebDriver bridge on 127.0.0.1:{bound} -> pid {} (bearer credential in {cred_path})", pid());
            if let Err(e) = s.serve(listener, &cred) { eprintln!("{e}"); std::process::exit(1) }
        }
        _ => eprintln!("usage: rightkit-control trusted|frontmost|launch|windows|ax|find|click|click-text|hover|drag|scroll|type|key|shot|serve <pid> ..."),
    }
}
