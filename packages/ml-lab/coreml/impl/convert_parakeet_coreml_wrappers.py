import torch
import torch.nn as nn


class EncoderWrap(nn.Module):
    """mel features (1, n_mel, T) -> encoded (1, D, T//8). Static T."""
    def __init__(self, model):
        super().__init__()
        self.encoder = model.encoder

    def prepare_static_trace(self, mel, length):
        with torch.no_grad():
            encoded, _ = self.encoder(audio_signal=mel, length=length)
        static_len = int(encoded.shape[-1])
        pos_enc = getattr(self.encoder, "pos_enc", None)
        if type(pos_enc).__name__ == "RelPositionalEncoding":
            self.encoder.pos_enc = StaticRelPositionalEncoding(pos_enc, static_len)
            print(f"[trace] fixed RelPositionalEncoding length={static_len}")

    def forward(self, mel, length):
        # VERIFY: NeMo ConformerEncoder.forward(audio_signal, length) -> (encoded, encoded_len)
        encoded, encoded_len = self.encoder(audio_signal=mel, length=length)
        return encoded  # (1, D, T')


class StaticRelPositionalEncoding(nn.Module):
    """Static-window replacement for NeMo RelPositionalEncoding.

    CoreMLTools cannot convert the tensor-derived Python int slice bounds in
    NeMo's dynamic relative position code. For fixed-window CoreML export, those
    bounds are constants, so keep the same PE slice as a buffer.
    """
    def __init__(self, original, input_len: int):
        super().__init__()
        self.xscale = original.xscale
        self.dropout = original.dropout
        self.dropout_emb = original.dropout_emb
        center_pos = int(original.pe.shape[1]) // 2 + 1
        start_pos = center_pos - input_len
        end_pos = center_pos + input_len - 1
        self.register_buffer("pos_emb", original.pe[:, start_pos:end_pos].detach().clone(), persistent=False)

    def forward(self, x, cache_len=0):
        if self.xscale:
            x = x * self.xscale
        pos_emb = self.pos_emb
        if self.dropout_emb:
            pos_emb = self.dropout_emb(pos_emb)
        return self.dropout(x), pos_emb


class DecoderWrap(nn.Module):
    """One prediction-network step: token + LSTM state -> pred + new state.
    Stateful, exactly like SW's TextDecoder (state_1/state_2)."""
    def __init__(self, model):
        super().__init__()
        self.decoder = model.decoder

    def forward(self, token, h, c):
        # token: (1,1) int32 ; h,c: (num_layers, 1, hidden)
        # VERIFY: NeMo RNNTDecoder.predict(y, state, add_sos, batch_size).
        # `predict` embeds `token`, runs the LSTM from (h,c), returns (g, new_state).
        g, (h2, c2) = self.decoder.predict(token.long(), [h, c], add_sos=False, batch_size=1)
        return g, h2, c2  # g: (1,1,D)


class JointWrap(nn.Module):
    """joint(enc_step (1,D), dec_step (1,D)) -> logits.
    TDT: returns (token_logits, duration_logits). RNN-T: token_logits only."""
    def __init__(self, model, is_tdt):
        super().__init__()
        self.joint = model.joint
        self.is_tdt = is_tdt

    def forward(self, enc_step, dec_step):
        # VERIFY: NeMo RNNTJoint.joint(f, g) expects (B, T, 1, D) / (B, 1, U, D);
        # we feed single steps (T=U=1). For TDT the last `num_durations` logits are
        # the duration head -- splitting is done host-side in coreml_bench.py, OR
        # here if joint already separates them.
        # NeMo RNNTJoint.joint expects f=[B,T,H1], g=[B,U,H2] (rank-3). Single step T=U=1.
        out = self.joint.joint(enc_step.unsqueeze(1), dec_step.unsqueeze(1))
        return out  # (1,1,1, vocab+1)


class MelWrap(nn.Module):
    """raw audio (1, T_samples) -> log-mel (1, n_mel, T_frames). Optional --
    STFT/FFT can be finicky in CoreML; --skip-mel does it host-side instead."""
    def __init__(self, model):
        super().__init__()
        self.preprocessor = model.preprocessor

    def forward(self, audio, length):
        # VERIFY: AudioToMelSpectrogramPreprocessor(input_signal, length) -> (feat, feat_len)
        feat, feat_len = self.preprocessor(input_signal=audio, length=length)
        return feat
