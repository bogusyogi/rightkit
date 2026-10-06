//! [PHASE-3 SCAFFOLD]
//!
//! Proposed runtime boundary if HeardRight later moves the full TDT state machine
//! into Rust. Do not force current CoreML/ORT code through this trait in Phase 1.

#[derive(Debug, Clone, PartialEq)]
pub struct BackendStep<S> {
    /// Token logits for the current encoder frame / predictor state.
    pub token_logits: Vec<f32>,
    /// TDT duration logits in the model's native index order.
    pub duration_logits: Vec<f32>,
    /// Predictor recurrent state *after* consuming the candidate input token.
    /// The adapter must validate this semantic against the current platform loop.
    pub emitted_state: S,
}

pub trait RecurrentTdtBackend {
    type State: Clone;
    type Error;

    fn initial_state(&mut self) -> Result<Self::State, Self::Error>;

    /// Produce token+duration logits for one hypothesis at one encoder frame.
    /// Exact blank/nonblank state retention remains an engine concern in Phase 3.
    fn step(
        &mut self,
        encoder_t: usize,
        last_token: usize,
        state: &Self::State,
    ) -> Result<BackendStep<Self::State>, Self::Error>;
}
