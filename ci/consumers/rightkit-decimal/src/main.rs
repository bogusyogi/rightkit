// rightkit-decimal 0.1.0 consumed by exact registry pin (`=0.1.0`), outside the workspace.
// Pure string arithmetic: no float, file, network or process is touched.
use rightkit_decimal::{
    compare, ordering, CompareError, Comparison, Decimal, DecimalError, MAX_DIGITS,
};
use std::cmp::Ordering;

fn check(name: &str, ok: bool) {
    if !ok {
        eprintln!("FAIL: {name}");
        std::process::exit(1);
    }
    println!("ok: {name}");
}

fn main() {
    // Canonical form: leading and trailing zeros drop; a bare point or sign form still parses.
    check(
        "canonical form",
        Decimal::parse("-012.5000").unwrap().to_string() == "-12.5",
    );
    check(
        "leading point",
        Decimal::parse(".5").unwrap().to_string() == "0.5",
    );
    check(
        "trailing point",
        Decimal::parse("7.").unwrap().to_string() == "7",
    );
    check(
        "explicit plus",
        Decimal::parse("+0.1").unwrap().to_string() == "0.1",
    );
    let negative_zero: Decimal = "-0.00".parse().unwrap();
    check(
        "negative zero is zero",
        negative_zero.is_zero() && !negative_zero.is_negative(),
    );
    check("negative flag", Decimal::parse("-3").unwrap().is_negative());

    // Exact equality and ordering with no floats.
    check(
        "1.0 equals 1",
        Decimal::parse("1.0").unwrap() == Decimal::parse("1").unwrap(),
    );
    check(
        "ordering of negatives",
        ordering("-2", "-1").unwrap() == Ordering::Less,
    );
    check(
        "ordering beyond float precision",
        ordering("0.1000000000000000000001", "0.1").unwrap() == Ordering::Greater,
    );
    check("compare ge", compare("10", Comparison::Ge, "9.99").unwrap());
    check(
        "compare lt",
        !compare("10", Comparison::Lt, "9.99").unwrap(),
    );
    check(
        "satisfies",
        Decimal::parse("2")
            .unwrap()
            .satisfies(Comparison::Eq, &Decimal::parse("2.00").unwrap()),
    );

    // Operator names.
    check(
        "operator word",
        Comparison::parse("LE") == Some(Comparison::Le),
    );
    check(
        "operator symbol",
        Comparison::parse(">=") == Some(Comparison::Ge),
    );
    check("unknown operator", Comparison::parse("~").is_none());
    check(
        "holds",
        Comparison::Gt.holds(Ordering::Greater) && !Comparison::Gt.holds(Ordering::Equal),
    );

    // Refusals.
    check(
        "empty refused",
        Decimal::parse("") == Err(DecimalError::Empty),
    );
    check(
        "bare sign refused",
        Decimal::parse("-") == Err(DecimalError::Empty),
    );
    check(
        "exponent refused",
        Decimal::parse("1e3") == Err(DecimalError::InvalidCharacter),
    );
    check(
        "two points refused",
        Decimal::parse("1.2.3") == Err(DecimalError::Malformed),
    );
    check(
        "misplaced sign refused",
        Decimal::parse("1-2") == Err(DecimalError::Malformed),
    );
    check(
        "too long refused",
        Decimal::parse(&"9".repeat(MAX_DIGITS + 1)) == Err(DecimalError::TooLong),
    );
    check(
        "left operand error",
        compare("x", Comparison::Eq, "1")
            == Err(CompareError::Left(DecimalError::InvalidCharacter)),
    );
    check(
        "right operand error",
        compare("1", Comparison::Eq, "") == Err(CompareError::Right(DecimalError::Empty)),
    );

    println!("OK rightkit-decimal 0.1.0 (registry consumer, public API)");
}
