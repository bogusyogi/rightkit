// rightkit-agent-engine-core 0.1.0 consumed by exact registry pin: secret redaction of a registered
// value (built at runtime, never a literal secret). No provider, network or process.
use rightkit_agent_engine_core::secret_redaction::SecretRedactor;

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

fn main() {
    let redactor = SecretRedactor::new();
    let value: String = ["tok", "en-", "consumer-", "0123456789abcdef"].concat();
    redactor.register_known(&value);
    let out = redactor.redact(&format!("key={value} done"));
    check(!out.contains(&value), "a registered value is redacted");
    check(out.ends_with(" done"), "surrounding text is kept");
    check(
        redactor.redact("plain text") == "plain text",
        "plain text passes through",
    );
}
