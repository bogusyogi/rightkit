mod interaction_gate_tests {
    use super::*;
    use rightkit_terminal_ui::component::Component;
    use rightkit_terminal_ui::transcript::timeline::UserQuestionPrompt;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct HostState {
        answers: Vec<(String, serde_json::Value)>,
        plans: Vec<(String, bool)>,
        answer_error: Option<String>,
        cancel_error: Option<String>,
        cancellations: usize,
    }

    #[derive(Clone, Default)]
    struct GateHost(Arc<Mutex<HostState>>);

    impl GateHost {
        fn answer_calls(&self) -> Vec<(String, serde_json::Value)> {
            self.0.lock().unwrap().answers.clone()
        }

        fn plan_calls(&self) -> Vec<(String, bool)> {
            self.0.lock().unwrap().plans.clone()
        }

        fn set_answer_error(&self, error: Option<&str>) {
            self.0.lock().unwrap().answer_error = error.map(str::to_owned);
        }

        fn set_cancel_error(&self, error: Option<&str>) {
            self.0.lock().unwrap().cancel_error = error.map(str::to_owned);
        }
    }

    impl TurnHost for GateHost {
        fn spawn_turn(&self, _prompt: String) -> JoinHandle<i32> {
            tokio::spawn(async { 0 })
        }

        async fn cancel_turn(&self) -> Result<(), String> {
            let mut state = self.0.lock().unwrap();
            state.cancellations += 1;
            if let Some(error) = state.cancel_error.clone() {
                Err(error)
            } else {
                Ok(())
            }
        }

        async fn answer_question(
            &self,
            question_id: &str,
            answers: serde_json::Value,
        ) -> Result<(), String> {
            let mut state = self.0.lock().unwrap();
            if let Some(error) = state.answer_error.clone() {
                return Err(error);
            }
            state.answers.push((question_id.to_owned(), answers));
            Ok(())
        }

        async fn resolve_plan(&self, plan_id: &str, approved: bool) -> Result<(), String> {
            self.0
                .lock()
                .unwrap()
                .plans
                .push((plan_id.to_owned(), approved));
            Ok(())
        }

        fn resolve_command(&self, _input: &str) -> Option<String> {
            None
        }
        fn check_command(&self, _name: &str, _mid_turn: bool) -> Result<(), String> {
            Ok(())
        }
        async fn execute_command(&self, _name: &str, _rest: &str) -> bool {
            false
        }
        fn completions(&self, _prefix: &str) -> Vec<String> {
            Vec::new()
        }
    }

    struct Fixture {
        composer: Composer,
        nav: NavView,
        session: SessionState,
        transcript: Transcript,
        surface: Surface,
        interjections: InterjectionBuffer,
        pending_echoes: std::collections::VecDeque<String>,
        turn: Option<JoinHandle<i32>>,
        requester: FrameRequester,
        exit_code: i32,
        should_exit: bool,
        viewed_agent: Option<AgentId>,
        paging: HistoryPaging,
    }

    impl Fixture {
        fn new() -> Self {
            let (draw_tx, _) = broadcast::channel(1);
            Self {
                composer: Composer::new(),
                nav: NavView::new(),
                session: SessionState::new(LOCAL_SESSION_ID),
                transcript: Transcript::new(),
                surface: Surface::new(
                    80,
                    24,
                    17,
                    7,
                    rightkit_terminal_ui::terminal::InsertMode::ScrollRegion,
                ),
                interjections: InterjectionBuffer::new(),
                pending_echoes: std::collections::VecDeque::new(),
                turn: None,
                requester: rightkit_terminal_ui::scheduler::spawn(draw_tx),
                exit_code: 0,
                should_exit: false,
                viewed_agent: None,
                paging: HistoryPaging::default(),
            }
        }

        async fn key(&mut self, host: &GateHost, code: KeyCode) {
            handle_event(
                host,
                &TuiEvent::Key(KeyEvent::new(code, KeyModifiers::NONE)),
                &mut self.composer,
                &mut self.nav,
                &mut self.session,
                &mut self.transcript,
                &mut self.surface,
                &self.interjections,
                &mut self.pending_echoes,
                &mut self.turn,
                &self.requester,
                &mut self.exit_code,
                &mut self.should_exit,
                &mut self.viewed_agent,
                &mut self.paging,
            )
            .await;
        }

        async fn paste(&mut self, host: &GateHost, text: String) {
            handle_event(
                host,
                &TuiEvent::Paste(text),
                &mut self.composer,
                &mut self.nav,
                &mut self.session,
                &mut self.transcript,
                &mut self.surface,
                &self.interjections,
                &mut self.pending_echoes,
                &mut self.turn,
                &self.requester,
                &mut self.exit_code,
                &mut self.should_exit,
                &mut self.viewed_agent,
                &mut self.paging,
            )
            .await;
        }

        fn question(&mut self, id: &str, prompts: Vec<UserQuestionPrompt>) {
            self.session.record(
                &mut self.transcript,
                DaemonEvent::UserQuestionAsked {
                    question_id: id.to_owned(),
                    questions: prompts,
                },
            );
            self.session.approval_input_safe = true;
        }

        fn plan(&mut self, id: &str, plan: &str) {
            self.session.record(
                &mut self.transcript,
                DaemonEvent::PlanPresented {
                    plan_id: id.to_owned(),
                    plan: plan.to_owned(),
                },
            );
            self.session.approval_input_safe = true;
        }
    }

    fn prompt(
        header: &str,
        question: &str,
        options: &[&str],
        multi_select: bool,
    ) -> UserQuestionPrompt {
        UserQuestionPrompt {
            header: header.to_owned(),
            question: question.to_owned(),
            options: options.iter().map(|value| (*value).to_owned()).collect(),
            descriptions: Vec::new(),
            multi_select,
        }
    }

    #[tokio::test]
    async fn numeric_free_text_is_not_an_option_index() {
        let host = GateHost::default();
        let mut fx = Fixture::new();
        fx.question("q1", vec![prompt("year", "Year", &["2025"], false)]);
        assert!(fx.session.interaction_text().is_some());
        for digit in "2026".chars() {
            fx.key(&host, KeyCode::Char(digit)).await;
        }
        fx.key(&host, KeyCode::Enter).await;
        assert_eq!(host.answer_calls()[0].1["year"], "2026");
    }

    #[tokio::test]
    async fn two_questions_commit_sequentially_with_multiselect() {
        let host = GateHost::default();
        let mut fx = Fixture::new();
        fx.question(
            "q2",
            vec![
                prompt("name", "Name", &[], false),
                prompt("colors", "Colors", &["red", "blue", "green"], true),
            ],
        );
        for ch in "Ada".chars() {
            fx.key(&host, KeyCode::Char(ch)).await;
        }
        fx.key(&host, KeyCode::Enter).await;
        assert!(host.answer_calls().is_empty());
        fx.key(&host, KeyCode::Char('1')).await;
        fx.key(&host, KeyCode::Char('3')).await;
        fx.key(&host, KeyCode::Enter).await;
        let answers = &host.answer_calls()[0].1;
        assert_eq!(answers["name"], "Ada");
        assert_eq!(answers["colors"], serde_json::json!(["red", "green"]));
    }

    #[tokio::test]
    async fn plan_y_then_enter_resolves() {
        let host = GateHost::default();
        let mut fx = Fixture::new();
        fx.plan("p1", "Apply patch");
        fx.key(&host, KeyCode::Char('y')).await;
        assert!(host.plan_calls().is_empty());
        fx.key(&host, KeyCode::Enter).await;
        assert_eq!(host.plan_calls(), vec![("p1".to_owned(), true)]);
    }

    #[tokio::test]
    async fn failed_answer_retains_payload_for_retry() {
        let host = GateHost::default();
        let mut fx = Fixture::new();
        fx.question("retry", vec![prompt("answer", "Answer", &[], false)]);
        for ch in "persist".chars() {
            fx.key(&host, KeyCode::Char(ch)).await;
        }
        host.set_answer_error(Some("answer failed"));
        fx.key(&host, KeyCode::Enter).await;
        assert!(fx.session.pending_question.is_some());
        host.set_answer_error(None);
        fx.key(&host, KeyCode::Enter).await;
        assert_eq!(host.answer_calls()[0].1["answer"], "persist");
    }

    #[tokio::test]
    async fn escape_successfully_cancels_question_and_plan() {
        let host = GateHost::default();
        let mut fx = Fixture::new();
        fx.turn = Some(tokio::spawn(async { std::future::pending::<i32>().await }));
        fx.question("cancel-q", vec![prompt("answer", "Answer", &[], false)]);
        fx.key(&host, KeyCode::Esc).await;
        // Esc declines the question: the turn keeps running and the model is told.
        assert!(fx.turn.is_some());
        assert_eq!(host.answer_calls()[0].1["__declined"], true);
        assert!(fx.session.pending_question.is_none());
        fx.turn.take().unwrap().abort();

        fx.turn = Some(tokio::spawn(async { std::future::pending::<i32>().await }));
        fx.plan("cancel-p", "Plan");
        fx.key(&host, KeyCode::Esc).await;
        assert!(fx.turn.is_none());
        assert!(fx.session.pending_plan.is_none());
        assert!(!fx.session.timeline.has_open_entry());
    }

    #[tokio::test]
    async fn escape_failed_cancellation_retains_gate() {
        let host = GateHost::default();
        host.set_cancel_error(Some("cancel failed"));
        let mut fx = Fixture::new();
        fx.turn = Some(tokio::spawn(async { std::future::pending::<i32>().await }));
        fx.plan("cancel-p", "Plan");
        fx.key(&host, KeyCode::Esc).await;
        assert!(fx.turn.is_some());
        assert!(fx.session.pending_plan.is_some());
        fx.turn.take().unwrap().abort();
    }

    #[tokio::test]
    async fn stale_input_is_swallowed_until_gate_is_safe() {
        let host = GateHost::default();
        let mut fx = Fixture::new();
        fx.question("stale", vec![prompt("answer", "Answer", &[], false)]);
        fx.session.approval_input_safe = false;
        fx.key(&host, KeyCode::Char('x')).await;
        assert_eq!(
            fx.session.pending_question.as_ref().unwrap().answer_text,
            ""
        );
        assert!(fx
            .session
            .interaction_text()
            .unwrap_or_default()
            .contains("Answer"));
    }

    #[tokio::test]
    async fn paste_feeds_question_answer_with_limit_and_never_composer() {
        let host = GateHost::default();
        let mut fx = Fixture::new();
        fx.question("paste", vec![prompt("answer", "Answer", &[], false)]);
        fx.paste(&host, "2026".to_owned()).await;
        assert_eq!(
            fx.session.pending_question.as_ref().unwrap().answer_text,
            "2026"
        );
        assert_eq!(fx.composer.cursor_line_text(), "");

        fx.paste(&host, "x".repeat(9000)).await;
        assert_eq!(
            fx.session
                .pending_question
                .as_ref()
                .unwrap()
                .answer_text
                .len(),
            8192
        );
        assert_eq!(fx.composer.cursor_line_text(), "");
    }

    #[tokio::test]
    async fn paste_in_plan_gate_does_not_select_or_submit() {
        let host = GateHost::default();
        let mut fx = Fixture::new();
        fx.plan("paste-plan", "Apply patch");
        fx.paste(&host, "y".to_owned()).await;
        assert_eq!(fx.session.pending_plan.as_ref().unwrap().decision, None);
        assert!(host.plan_calls().is_empty());
        assert_eq!(fx.composer.cursor_line_text(), "");
    }

    #[tokio::test]
    async fn delayed_paste_tick_waits_for_queued_input_then_flushes_one_burst() {
        let mut composer = Composer::new();
        for ch in "P".repeat(512).chars() {
            composer.handle(&TuiEvent::Key(KeyEvent::new(
                KeyCode::Char(ch),
                KeyModifiers::NONE,
            )));
        }
        assert!(composer.is_in_paste_burst());

        let (tx, mut rx) = mpsc::channel(3584);
        for _ in 0..3584 {
            tx.send(Ok(rightkit_terminal_ui::crossterm::event::Event::Key(
                KeyEvent::new(KeyCode::Char('P'), KeyModifiers::NONE),
            )))
            .await
            .unwrap();
        }
        drop(tx);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(!flush_paste_burst_if_input_drained(&mut composer, &rx));
        assert!(composer.is_in_paste_burst());

        composer.set_defer_paste_burst_idle_flush(true);
        while let Some(Ok(rightkit_terminal_ui::crossterm::event::Event::Key(key))) = rx.recv().await {
            composer.handle(&TuiEvent::Key(key));
        }
        composer.set_defer_paste_burst_idle_flush(false);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(flush_paste_burst_if_input_drained(&mut composer, &rx));
        assert_eq!(composer.cursor_line_text(), "[Pasted Content 4096 chars]");
        composer.handle(&TuiEvent::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        let intents = composer.drain_intents();
        assert!(matches!(
            intents.as_slice(),
            [ComposerIntent::Submit(text)]
                if text.len() == 4096 && text.chars().all(|character| character == 'P')
        ));

        let mut idle = Composer::new();
        for character in "idle".chars() {
            idle.handle(&TuiEvent::Key(KeyEvent::new(
                KeyCode::Char(character),
                KeyModifiers::NONE,
            )));
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(idle.flush_paste_burst_if_due());

        let mut held = Composer::new();
        held.set_defer_paste_burst_idle_flush(true);
        held.handle(&TuiEvent::Key(KeyEvent::new(
            KeyCode::Char('P'),
            KeyModifiers::NONE,
        )));
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        held.handle(&TuiEvent::Key(KeyEvent::new(
            KeyCode::Char('P'),
            KeyModifiers::NONE,
        )));
        held.set_defer_paste_burst_idle_flush(false);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(held.flush_paste_burst_if_due());
        assert_eq!(held.cursor_line_text(), "PP");
    }
}

mod goal_and_subagent_message_tests {
    use super::*;
    use crate::tui_projects::{goal_chip, goal_status_text, parse_goal_args, GoalCmd};
    use serde_json::json;

    #[test]
    fn goal_args_cover_set_show_pause_resume_clear() {
        assert_eq!(parse_goal_args("").unwrap(), GoalCmd::Show);
        assert_eq!(
            parse_goal_args("set ship it").unwrap(),
            GoalCmd::Set("ship it".into())
        );
        assert_eq!(
            parse_goal_args("ship it").unwrap(),
            GoalCmd::Set("ship it".into())
        );
        assert_eq!(
            parse_goal_args("edit new text").unwrap(),
            GoalCmd::Edit("new text".into())
        );
        assert_eq!(parse_goal_args("pause").unwrap(), GoalCmd::Pause);
        assert_eq!(parse_goal_args("RESUME").unwrap(), GoalCmd::Resume);
        assert_eq!(parse_goal_args("clear").unwrap(), GoalCmd::Clear);
        assert!(parse_goal_args("set").is_err());
        assert!(parse_goal_args("edit").is_err());
    }

    #[test]
    fn goal_chip_projects_status_progress_and_hides_when_absent() {
        let body = json!({"goal": {"status": "paused", "objective": "fix the build", "completionPercent": 40}});
        assert_eq!(
            goal_chip(&body).unwrap(),
            "goal (paused): fix the build · 40%"
        );
        let steps = json!({"status": "active", "objective": "x", "continuationCount": 3});
        assert_eq!(goal_chip(&steps).unwrap(), "goal (active): x · step 3");
        assert!(goal_chip(&json!({"goal": null})).is_none());
        assert!(goal_chip(&json!({"goal": {"objective": "x", "dismissedAt": "t"}})).is_none());
        let long = json!({"objective": "a".repeat(80)});
        assert!(goal_chip(&long).unwrap().chars().count() < 70);
    }

    #[test]
    fn goal_status_card_lists_objective_now_next_or_empty_hint() {
        let body = json!({"goal": {"objective": "ship", "now": "testing", "next": "release"}});
        let text = goal_status_text(&body);
        assert!(
            text.contains("Objective: ship")
                && text.contains("Now: testing")
                && text.contains("Next: release")
        );
        assert!(goal_status_text(&json!({"goal": null})).contains("/goal set"));
    }

    #[test]
    fn subagent_follow_up_notice_reports_delivery_and_failure() {
        let agent = AgentId("child-1".into());
        assert_eq!(
            subagent_message_notice(&agent, Ok(())),
            "Sent follow-up to agent child-1."
        );
        let failed = subagent_message_notice(&agent, Err("HTTP 404".into()));
        assert!(failed.contains("not delivered") && failed.contains("HTTP 404"));
    }
}
