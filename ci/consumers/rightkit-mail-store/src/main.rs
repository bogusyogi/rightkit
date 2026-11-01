//! Registry consumer for rightkit-mail-store 0.1.0: public API only, no database key and no I/O.
use rightkit_mail_store::db_schema::SCHEMA_VERSION;
use rightkit_mail_store::host::{Channel, DirKind};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    check(SCHEMA_VERSION > 0, "the schema has a version");
    check(
        Channel::Release.as_str() == "release" && Channel::Dev.as_str() == "dev",
        "channel names are stable",
    );
    check(
        DirKind::Default != DirKind::Override,
        "the data folder kinds differ",
    );
}
