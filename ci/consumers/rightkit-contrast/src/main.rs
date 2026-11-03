// rightkit-contrast 0.1.0 consumed by exact registry pin (`=0.1.0`), outside the workspace.
// Pure colour arithmetic: no file, network, process or display is touched.
use rightkit_contrast::{contrast_ratio, is_light, readable_on, relative_luminance, Rgb};

fn near(actual: f64, expected: f64) -> bool {
    (actual - expected).abs() < 1e-9
}

fn check(name: &str, ok: bool) {
    if !ok {
        eprintln!("FAIL: {name}");
        std::process::exit(1);
    }
    println!("ok: {name}");
}

fn main() {
    // Hex parsing: short form, long form, alpha discarded, whitespace trimmed, refusals.
    check(
        "short hex expands",
        Rgb::from_hex("#fff") == Some(Rgb::WHITE),
    );
    check(
        "long hex parses case-insensitively",
        Rgb::from_hex("#1a1208") == Some(Rgb::DEFAULT_DARK),
    );
    check(
        "alpha channel is discarded",
        Rgb::from_hex("  #FF000080 ") == Some(Rgb::new(0xFF, 0, 0)),
    );
    check("hash is required", Rgb::from_hex("FFFFFF").is_none());
    check("bad length is refused", Rgb::from_hex("#FFFF").is_none());
    check(
        "non-hex digits are refused",
        Rgb::from_hex("#GGGGGG").is_none(),
    );
    check(
        "to_hex is upper case",
        Rgb::new(0x1A, 0x12, 0x08).to_hex() == "#1A1208",
    );

    // WCAG luminance endpoints and the maximum contrast ratio.
    check(
        "black luminance is 0",
        near(relative_luminance(Rgb::BLACK), 0.0),
    );
    check(
        "white luminance is 1",
        near(relative_luminance(Rgb::WHITE), 1.0),
    );
    check(
        "black on white contrast is 21",
        near(contrast_ratio(Rgb::BLACK, Rgb::WHITE), 21.0),
    );
    check(
        "contrast ratio is symmetric",
        near(
            contrast_ratio(Rgb::new(10, 120, 200), Rgb::WHITE),
            contrast_ratio(Rgb::WHITE, Rgb::new(10, 120, 200)),
        ),
    );
    check(
        "identical colours have ratio 1",
        near(contrast_ratio(Rgb::new(7, 8, 9), Rgb::new(7, 8, 9)), 1.0),
    );

    // Readable ink: the documented yellow takes the dark ink; a dark background takes the light ink.
    let yellow = Rgb::from_hex("#F2C200").unwrap();
    check(
        "dark ink on yellow",
        readable_on(yellow, Rgb::WHITE, Rgb::DEFAULT_DARK) == Rgb::DEFAULT_DARK,
    );
    check(
        "light ink on black",
        readable_on(Rgb::BLACK, Rgb::WHITE, Rgb::DEFAULT_DARK) == Rgb::WHITE,
    );
    check("white background is light", is_light(Rgb::WHITE));
    check("black background is not light", !is_light(Rgb::BLACK));

    println!("OK rightkit-contrast 0.1.0 (registry consumer, public API)");
}
