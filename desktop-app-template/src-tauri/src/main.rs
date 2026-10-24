// The GUI subsystem on Windows in every build, dev and release alike: a console window beside an installed app is never wanted
// (the Cargo profile contract refuses a release-only attribute). The attribute is ignored on other platforms.
#![windows_subsystem = "windows"]

fn main() {
    rightapp::run();
}
