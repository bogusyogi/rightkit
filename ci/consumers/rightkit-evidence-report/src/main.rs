// rightkit-evidence-report consumed by the exact registry pin, outside the workspace.
// Proves: neutral default branding, first-call-wins branding, HTML and Markdown escaping.
use rightkit_evidence_report::{escape, report_branding, set_report_branding, ReportBranding};

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

fn main() {
    check(
        report_branding().tool_name == "RightKit",
        "neutral default SARIF tool name",
    );
    check(
        !set_report_branding(ReportBranding::default()),
        "branding is fixed after first use",
    );
    check(
        escape::html("<a href=\"x\">&</a>") == "&lt;a href=&quot;x&quot;&gt;&amp;&lt;/a&gt;",
        "html escaping",
    );
    check(
        !escape::markdown("*bold*").contains("*bold*"),
        "markdown escaping",
    );
}
