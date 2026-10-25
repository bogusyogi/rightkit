// Unconditional: a dev build has debug assertions on (the single Cargo `dev` profile), and an installed dev app
// must not open a console window. The attribute only has an effect on Windows targets.
#![windows_subsystem = "windows"]

fn main() {
    rightapp::run();
}
