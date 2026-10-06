//! The graph fragment the render path inserts on the dialogue bus.

use crate::chain::DialogueChain;
use crate::error::AudioError;
use crate::filter::render_chain;

/// Textual fragment ready to join into `-filter_complex` with `;`.
pub type GraphFragment = String;

/// Build the dialogue-bus fragment:
///
/// ```text
/// [dialogue_in] highpass, denoise, acompressor, deesser  [prefix_d]
/// [prefix_d][room_tone_layer] amix(normalize=0)           [prefix_m]   (only with a layer)
/// [prefix_m] alimiter                                     [out]
/// ```
///
/// `dialogue_in` is the assembled dialogue (cuts applied, before music and before the
/// final `loudnorm`). `room_tone_layer_in` is a full-length room-tone bed (see
/// [`crate::room_tone::room_tone_fill_fragment`] / [`crate::measure::AudioTools::render_room_tone_layer`]);
/// it is mixed before the limiter so the ceiling holds for the sum. Labels are given
/// without brackets.
pub fn dialogue_bus_fragment(
    chain: &DialogueChain,
    dialogue_in: &str,
    room_tone_layer_in: Option<&str>,
    out: &str,
    prefix: &str,
) -> Result<GraphFragment, AudioError> {
    chain.validate()?;
    let mut pre = chain.conditioning_filters();
    pre.extend(chain.dynamics_filters());
    let limiter = chain.limiter_filter().render();
    match room_tone_layer_in {
        None => {
            pre.push(chain.limiter_filter());
            Ok(format!("[{dialogue_in}]{}[{out}]", render_chain(&pre)))
        }
        Some(layer) => {
            if pre.is_empty() {
                pre.push(crate::filter::Filter::new("anull"));
            }
            Ok(format!(
                "[{dialogue_in}]{}[{prefix}_d];[{prefix}_d][{layer}]amix=inputs=2:normalize=0:duration=first[{prefix}_m];[{prefix}_m]{limiter}[{out}]",
                render_chain(&pre)
            ))
        }
    }
}
