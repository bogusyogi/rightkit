//! The destructive-action guard — intercepts dangerous tool calls before execution.

/// A guard decision for an intercepted tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DestructiveGuardDecision {
    /// The action is safe to run.
    Allow,
    /// The action is destructive and must be confirmed by the user.
    RequireConfirmation { reason: String },
    /// The action is forbidden outright.
    Block { reason: String },
}

/// Command injection signal types detected in shell commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InjectionSignal {
    /// Backtick command substitution: `cmd`
    BacktickSubstitution,
    /// Dollar-paren command substitution: $(cmd)
    DollarParenSubstitution,
    /// Unquoted variable as command argument: $VAR in unquoted context
    UnquotedVariable,
    /// Piped to privileged command: | sudo, | tee /etc
    PipeToPrivileged,
    /// Process substitution: <(cmd) or >(cmd)
    ProcessSubstitution,
    /// Here-string injection: <<< $VAR
    HereStringInjection,
}

/// MITRE ATT&CK technique mapping for a detected signal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttackMapping {
    pub signal: InjectionSignal,
    /// MITRE ATT&CK technique ID (e.g. "T1059.004")
    pub technique_id: &'static str,
    /// MITRE ATT&CK technique name
    pub technique_name: &'static str,
}

/// Intercepts shell commands and file operations that could cause irreversible damage.
pub struct DestructiveGuard {
    /// Extra command substrings that should be blocked outright.
    blocked_patterns: Vec<String>,
}

impl DestructiveGuard {
    pub fn new() -> Self {
        Self {
            blocked_patterns: Vec::new(),
        }
    }

    /// Add a custom blocked pattern (case-insensitive substring match on the command).
    pub fn block_pattern(mut self, pattern: impl Into<String>) -> Self {
        self.blocked_patterns.push(pattern.into().to_lowercase());
        self
    }

    /// Analyze a shell command for injection signals and map to MITRE ATT&CK.
    pub fn analyze_injection(&self, command: &str) -> Vec<AttackMapping> {
        let mut signals = Vec::new();

        // Detect backtick substitution: `...`
        if self.contains_backtick_substitution(command) {
            signals.push(AttackMapping {
                signal: InjectionSignal::BacktickSubstitution,
                technique_id: "T1059.004",
                technique_name: "Unix Shell",
            });
        }

        // Detect dollar-paren substitution: $(...)
        if self.contains_dollar_paren_substitution(command) {
            signals.push(AttackMapping {
                signal: InjectionSignal::DollarParenSubstitution,
                technique_id: "T1059.004",
                technique_name: "Unix Shell",
            });
        }

        // Detect unquoted variables in potentially dangerous contexts
        if self.contains_unquoted_variable(command) {
            signals.push(AttackMapping {
                signal: InjectionSignal::UnquotedVariable,
                technique_id: "T1078",
                technique_name: "Valid Accounts",
            });
        }

        // Detect pipe to privileged commands
        if self.contains_pipe_to_privileged(command) {
            signals.push(AttackMapping {
                signal: InjectionSignal::PipeToPrivileged,
                technique_id: "T1548.003",
                technique_name: "Abuse Elevation Control Mechanism: Sudo",
            });
        }

        // Detect process substitution: <(...) or >(...)
        if self.contains_process_substitution(command) {
            signals.push(AttackMapping {
                signal: InjectionSignal::ProcessSubstitution,
                technique_id: "T1059.004",
                technique_name: "Unix Shell",
            });
        }

        // Detect here-string with variable: <<< $VAR
        if self.contains_here_string_injection(command) {
            signals.push(AttackMapping {
                signal: InjectionSignal::HereStringInjection,
                technique_id: "T1059.004",
                technique_name: "Unix Shell",
            });
        }

        signals
    }

    /// Check if command contains backtick command substitution.
    fn contains_backtick_substitution(&self, command: &str) -> bool {
        let mut in_backtick = false;
        for ch in command.chars() {
            match ch {
                '`' => in_backtick = !in_backtick,
                _ => {
                    if in_backtick {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Check if command contains $(...) substitution.
    fn contains_dollar_paren_substitution(&self, command: &str) -> bool {
        let bytes = command.as_bytes();
        for i in 0..bytes.len().saturating_sub(1) {
            if bytes[i] == b'$' && bytes[i + 1] == b'(' {
                return true;
            }
        }
        false
    }

    /// Check if command contains unquoted variables (simple heuristic).
    /// Looks for $VAR patterns outside of quotes.
    fn contains_unquoted_variable(&self, command: &str) -> bool {
        let mut in_single_quote = false;
        let mut in_double_quote = false;
        let mut prev_char = ' ';

        for ch in command.chars() {
            match ch {
                '\'' if !in_double_quote => in_single_quote = !in_single_quote,
                '"' if !in_single_quote => in_double_quote = !in_double_quote,
                '$' if !in_single_quote && !in_double_quote && prev_char.is_whitespace() => {
                    return true;
                }
                _ => {}
            }
            prev_char = ch;
        }
        false
    }

    /// Check if command pipes output to privileged commands (sudo, tee /etc, tee /usr).
    fn contains_pipe_to_privileged(&self, command: &str) -> bool {
        let lower = command.to_lowercase();
        lower.contains("| sudo")
            || lower.contains("| su ")
            || lower.contains("| tee /etc")
            || lower.contains("| tee /usr")
    }

    /// Check if command contains process substitution: <(...) or >(...)
    fn contains_process_substitution(&self, command: &str) -> bool {
        let bytes = command.as_bytes();
        for i in 0..bytes.len().saturating_sub(1) {
            if (bytes[i] == b'<' || bytes[i] == b'>') && bytes[i + 1] == b'(' {
                return true;
            }
        }
        false
    }

    /// Check if command contains here-string injection: <<< followed by $
    fn contains_here_string_injection(&self, command: &str) -> bool {
        let bytes = command.as_bytes();
        for i in 0..bytes.len().saturating_sub(3) {
            if bytes[i] == b'<' && bytes[i + 1] == b'<' && bytes[i + 2] == b'<' {
                // Look for $ within next few chars (skip whitespace)
                for &b in &bytes[(i + 3)..] {
                    match b {
                        b' ' | b'\t' => continue,
                        b'$' => return true,
                        _ => break,
                    }
                }
            }
        }
        false
    }

    /// Check if command contains potential taint flow indicators.
    /// A taint flow indicator is a shell metacharacter sequence that could
    /// allow an injected value to escape its intended context.
    pub fn has_taint_indicators(&self, command: &str) -> bool {
        let mut in_single_quote = false;
        let mut in_double_quote = false;

        for ch in command.chars() {
            match ch {
                '\'' if !in_double_quote => {
                    in_single_quote = !in_single_quote;
                }
                '"' if !in_single_quote => {
                    in_double_quote = !in_double_quote;
                }
                _ => {}
            }
        }

        let unbalanced_quotes = in_single_quote || in_double_quote;

        if unbalanced_quotes {
            // Check for shell metacharacters that create taint flows
            const METACHARACTERS: &[char] = &[
                ';', '|', '&', '<', '>', '`', '$', '(', ')', '{', '}', '[', ']', '*', '?', '!', '~',
            ];
            for ch in command.chars() {
                if METACHARACTERS.contains(&ch) {
                    return true;
                }
            }
        }

        false
    }

    /// Evaluate a shell command for destructiveness.
    pub fn evaluate_shell(&self, command: &str) -> DestructiveGuardDecision {
        let lower = command.to_lowercase();

        // Custom blocks take priority.
        for pat in &self.blocked_patterns {
            if lower.contains(pat) {
                return DestructiveGuardDecision::Block {
                    reason: format!("command matches blocked pattern: {pat}"),
                };
            }
        }

        // Hard blocks — catastrophic, never allowed.
        const HARD_BLOCKS: &[(&str, &str)] = &[
            ("rm -rf /", "recursive delete of filesystem root"),
            (":(){ :|:& };:", "fork bomb"),
            ("mkfs", "filesystem format"),
            ("dd if=", "raw disk write"),
            ("> /dev/sda", "raw disk overwrite"),
        ];
        for (needle, reason) in HARD_BLOCKS {
            if lower.contains(needle) {
                return DestructiveGuardDecision::Block {
                    reason: reason.to_string(),
                };
            }
        }

        // Soft warns — destructive but legitimate; require confirmation.
        const SOFT_WARNS: &[(&str, &str)] = &[
            ("rm -rf", "recursive force delete"),
            ("rm -r", "recursive delete"),
            ("git reset --hard", "discards uncommitted changes"),
            ("git clean -f", "deletes untracked files"),
            ("git push --force", "force-push rewrites remote history"),
            ("git push -f", "force-push rewrites remote history"),
            ("truncate", "truncates a file"),
            ("chmod -r", "recursive permission change"),
            ("chown -r", "recursive ownership change"),
            ("drop table", "destructive SQL"),
            ("drop database", "destructive SQL"),
        ];
        for (needle, reason) in SOFT_WARNS {
            if lower.contains(needle) {
                return DestructiveGuardDecision::RequireConfirmation {
                    reason: reason.to_string(),
                };
            }
        }

        // Level 2: Injection analysis
        let injection_signals = self.analyze_injection(command);
        if !injection_signals.is_empty() {
            let techniques: Vec<_> = injection_signals
                .iter()
                .map(|m| format!("{} ({})", m.technique_name, m.technique_id))
                .collect();
            return DestructiveGuardDecision::RequireConfirmation {
                reason: format!(
                    "potential shell injection detected: {}",
                    techniques.join("; ")
                ),
            };
        }

        // Level 3: Taint flow check
        if self.has_taint_indicators(command) {
            return DestructiveGuardDecision::RequireConfirmation {
                reason: "unquoted variable or taint flow in shell command".into(),
            };
        }

        DestructiveGuardDecision::Allow
    }
}

impl Default for DestructiveGuard {
    fn default() -> Self {
        Self::new()
    }
}
