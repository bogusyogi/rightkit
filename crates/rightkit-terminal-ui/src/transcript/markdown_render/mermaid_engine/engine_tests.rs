//! Authored boundary tests for the absorbed donor renderer. These assert
//! observable contract behavior (width, resource limits, unsupported-syntax
//! fallback, no terminal escapes), not mirrored donor implementation tests.

use super::RenderError;
use super::Role;
use super::render_spans;
use unicode_width::UnicodeWidthStr;

fn text(diagram: &[Vec<super::Span>]) -> String {
    diagram
        .iter()
        .map(|line| {
            line.iter()
                .map(|span| span.text.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn flowchart_renders_boxes_and_directed_edge() {
    let diagram = render_spans("flowchart LR\nA[In] --> B[Out]", 120).expect("render");
    let output = text(&diagram);
    assert!(output.contains("In"));
    assert!(output.contains("Out"));
    assert!(output.contains('┌') && output.contains('┘'));
    assert!(
        diagram
            .iter()
            .flatten()
            .any(|span| span.role == Role::Edge && span.text.contains('─')),
        "edge lane must be drawn: {output}"
    );
    for role in [Role::Node, Role::Edge, Role::Text] {
        assert!(
            diagram.iter().flatten().any(|span| span.role == role),
            "role {role:?} must appear: {output}"
        );
    }
}

#[test]
fn rendered_rows_never_exceed_max_width() {
    for source in [
        "flowchart LR\nA[Request] --> B[Reply] --> C[Done]",
        "flowchart TD\nA --> B\nA --> C",
        "sequenceDiagram\nA->>B: request\nB-->>A: response",
        "stateDiagram-v2\n[*] --> Active\nActive --> [*]",
        "classDiagram\nOrder \"1\" *-- \"many\" Item : contains",
        "erDiagram\nCUSTOMER ||--o{ ORDER : places",
    ] {
        let diagram = render_spans(source, 100).unwrap_or_else(|e| panic!("{source}: {e:?}"));
        for line in &diagram {
            let width = line
                .iter()
                .map(|span| UnicodeWidthStr::width(span.text.as_str()))
                .sum::<usize>();
            assert!(width <= 100, "{source} row exceeds width: {width}");
        }
    }
}

#[test]
fn narrow_width_reports_too_wide() {
    assert_eq!(
        render_spans("flowchart LR\nA[Request] --> B[Reply]", 8),
        Err(RenderError::TooWide)
    );
    assert_eq!(
        render_spans("sequenceDiagram\nAlice->>Bob: hello", 10),
        Err(RenderError::TooWide)
    );
}

#[test]
fn oversized_source_reports_limit() {
    let source = format!("flowchart TD\n{}", "A --> B\n".repeat(3000));
    assert!(source.len() > 16 * 1024);
    assert_eq!(render_spans(&source, 120), Err(RenderError::Limit));
}

#[test]
fn oversized_label_and_node_count_report_limit() {
    assert_eq!(
        render_spans(
            "flowchart TD\nA[This label exceeds the forty column limit]",
            120
        ),
        Err(RenderError::Limit)
    );
    // The node cap is 16; a chain of 17 distinct nodes must fail intact.
    let chain = (0..17)
        .map(|i| format!("N{i}"))
        .collect::<Vec<_>>()
        .join(" --> ");
    assert_eq!(
        render_spans(&format!("flowchart LR\n{chain}"), 400),
        Err(RenderError::Limit)
    );
}

#[test]
fn unsupported_syntax_and_text_fail_intact() {
    for source in [
        // Unknown diagram family.
        "pie\n\"Cats\": 2",
        // Frontmatter directives are outside the subset.
        "%%{init: {}}%%\nflowchart TD\nA --> B",
        // Undecoded entities must not become literal text.
        "sequenceDiagram\nA->>B: &amp;",
        // Markup requires layout this renderer does not do.
        "flowchart TD\nA[<b>bold</b>] --> B",
        // Reserved flowchart keywords are not node IDs.
        "flowchart TD\nsubgraph X\nA --> B\nend",
        // Unsupported node shape after a supported edge.
        "flowchart TD\nP --> Q\nA[(Database)]",
        // Markdown strings inside labels.
        "flowchart TD\nP --> Q\nA[\"`hello **world**`\"]",
        // Unbalanced label delimiter.
        "flowchart LR\nA[unfinished",
        // Control characters are unsafe terminal text.
        "flowchart TD\nA[bad\x07label] --> B",
        // Unterminated sequence frame.
        "sequenceDiagram\nalt ok\nA->>B: hi",
    ] {
        assert_eq!(
            render_spans(source, 120),
            Err(RenderError::Unsupported),
            "{source:?}"
        );
    }
}

#[test]
fn output_contains_no_terminal_control_characters() {
    let diagram =
        render_spans("flowchart LR\nA[请求] --> B[Réponse] --> C{Done?}", 120).expect("render");
    for line in &diagram {
        for span in line {
            assert!(
                !span.text.chars().any(|ch| ch.is_control()),
                "control char in {:?}",
                span.text
            );
        }
    }
}

#[test]
fn sequence_diagram_renders_lifelines_and_messages() {
    let output = text(
        &render_spans(
            "sequenceDiagram\nparticipant A as Alice\nA->>B: request\nB-->>A: response",
            120,
        )
        .expect("render"),
    );
    assert!(output.contains("Alice"));
    assert!(output.contains("request"));
    assert!(output.contains("response"));
    assert!(output.contains('▶'));
}

#[test]
fn wide_unicode_labels_render_aligned() {
    // CJK scalars are two cells wide; the box must close on the same row.
    let diagram = render_spans("flowchart TD\nA[请求] --> B[応答]", 120).expect("render");
    let output = text(&diagram);
    assert!(output.contains("请求"));
    for line in &output.lines().collect::<Vec<_>>() {
        if line.contains("请求") || line.contains("応答") {
            let first = line.find('│').unwrap();
            let last = line.rfind('│').unwrap();
            assert!(last > first, "box sides must bracket content: {line}");
        }
    }
}
