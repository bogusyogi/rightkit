"""PyTorch twin of silero_vad_16k_op15.onnx at HR's fixed shapes.

Fixed contract (matches heardright-engine/src/vad.rs):
  input  [1, 576] f32  (64 context + 512 frame, clamped upstream)
  state  [2, 1, 128] f32 ([h; c])
  ->
  output [1, 1] f32 (speech probability)
  stateN [2, 1, 128] f32

The ONNX graph's If/Shape branches all collapse at these shapes; sr only
feeds `stateN = concat(h, c) + sr*0` (a keep-input-alive trick), so it is
dropped here. Weights are read straight from the ONNX initializers.
"""

import numpy as np
import onnx
import torch
import torch.nn.functional as F
from onnx import numpy_helper

INPUT_SAMPLES = 576


def load_weights(onnx_path):
    m = onnx.load(str(onnx_path))
    return {t.name: torch.from_numpy(numpy_helper.to_array(t).copy()) for t in m.graph.initializer}


class SileroVad16k(torch.nn.Module):
    def __init__(self, w):
        super().__init__()
        self.stft_basis = torch.nn.Parameter(w["model.stft.forward_basis_buffer"], requires_grad=False)
        for i in range(4):
            setattr(self, f"enc{i}_w", torch.nn.Parameter(w[f"model.encoder.{i}.reparam_conv.weight"], requires_grad=False))
            setattr(self, f"enc{i}_b", torch.nn.Parameter(w[f"model.encoder.{i}.reparam_conv.bias"], requires_grad=False))
        self.w_ih = torch.nn.Parameter(w["model.decoder.rnn.weight_ih"], requires_grad=False)
        self.w_hh = torch.nn.Parameter(w["model.decoder.rnn.weight_hh"], requires_grad=False)
        self.b_ih = torch.nn.Parameter(w["model.decoder.rnn.bias_ih"], requires_grad=False)
        self.b_hh = torch.nn.Parameter(w["model.decoder.rnn.bias_hh"], requires_grad=False)
        self.out_w = torch.nn.Parameter(w["model.decoder.decoder.2.weight"], requires_grad=False)
        self.out_b = torch.nn.Parameter(w["model.decoder.decoder.2.bias"], requires_grad=False)

    def forward(self, x, state):
        # x [1,576], state [2,1,128]
        x = F.pad(x, (0, 64), mode="reflect")            # [1,640] right-only reflect
        spec = F.conv1d(x.unsqueeze(1), self.stft_basis, stride=128)  # [1,258,4]
        real, imag = spec[:, :129], spec[:, 129:]
        mag = torch.sqrt(real * real + imag * imag)      # [1,129,4]
        h = F.relu(F.conv1d(mag, self.enc0_w, self.enc0_b, padding=1))            # [1,128,4]
        h = F.relu(F.conv1d(h, self.enc1_w, self.enc1_b, stride=2, padding=1))    # [1,64,2]
        h = F.relu(F.conv1d(h, self.enc2_w, self.enc2_b, stride=2, padding=1))    # [1,64,1]
        h = F.relu(F.conv1d(h, self.enc3_w, self.enc3_b, padding=1))              # [1,128,1]
        xt = h[:, :, 0]                                   # [1,128] single LSTM step
        h0, c0 = state[0], state[1]                       # [1,128] each
        gates = xt @ self.w_ih.T + self.b_ih + h0 @ self.w_hh.T + self.b_hh
        i, f, g, o = gates.chunk(4, dim=1)                # onnx LSTM iofc? see note
        i, f, o = torch.sigmoid(i), torch.sigmoid(f), torch.sigmoid(o)
        g = torch.tanh(g)
        c1 = f * c0 + i * g
        h1 = o * torch.tanh(c1)
        y = F.relu(h1).unsqueeze(2)                       # [1,128,1]
        y = torch.sigmoid(F.conv1d(y, self.out_w, self.out_b))  # [1,1,1]
        prob = y.mean(dim=2)                              # [1,1]
        state_n = torch.stack([h1, c1], dim=0)            # [2,1,128]
        return prob, state_n
