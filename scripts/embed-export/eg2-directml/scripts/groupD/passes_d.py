"""Group D passes: fewer-node RMS norm via LpNormalization, single-node Gelu, constant-scale merge,
and an incremental shared-data writer (several static lengths share one model.onnx_data).

Reuses groupA/passes.py for gelu, rope, stable_norm, embeds, static, cse, dce, _rewrite, _raw.

  lp_norm(model, mode)   SimplifiedLayerNormalization / SkipSimplifiedLayerNormalization ->
                         [Add (skip only)] -> [mode-specific pre-scale] -> LpNormalization(axis=-1,p=2)
                         -> Mul(gamma*sqrt(D)).
                         mode "plain"   : no pre-scale (D26, D32)
                         mode "maxabs"  : u = x / ReduceMax(Abs(x), -1)   (D28, D30, D31)
                         mode "prescale": u = x * (1/64)                  (D29)
  gelu_none(model)       ai.onnx Gelu: approximate attribute set to "none" (erf form), one node each.
  merge_const_scale(m)   Mul/Div by a constant that feeds another Mul/Div by a constant (single
                         consumer, not a graph output) -> one Mul with the product constant.
  SharedWriter           save_shared() made incremental so models can be written one at a time.
"""
import copy  # noqa: F401  (kept for callers that import this module as a toolbox)
import hashlib
import math
import os
import sys
from collections import Counter

import numpy as np
import onnx
from onnx import helper, numpy_helper
from onnx.external_data_helper import set_external_data

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(os.path.dirname(HERE), "groupA"))
import passes as P  # noqa: E402

F32 = np.float32
I64 = np.int64
NORM_OPS = ("SimplifiedLayerNormalization", "SkipSimplifiedLayerNormalization")
MODES = ("plain", "maxabs", "prescale")


# ----------------------------------------------------------------- pass: lp_norm

def lp_norm(model, mode):
    """RMS norm x/sqrt(mean(x^2)+eps)*gamma  ==  LpNormalization(x)*gamma*sqrt(D) without eps.

    LpNormalization is scale invariant, so the max-abs (or 1/64) pre-scale changes only the
    fp16 range of the squared terms, not the value. Returns the number of norm nodes rewritten.
    """
    assert mode in MODES, mode
    g = model.graph
    inits = {t.name: t for t in g.initializer}
    stats = {"scale_const_overflow_fp16": 0, "scalar_scales": 0}

    def build(n, b):
        if n.op_type == "SimplifiedLayerNormalization":
            assert int(P.attr(n, "axis", -1)) == -1, "only last-axis norm is handled"
            assert not any(n.output[1:]), "auxiliary inv_std_var output is not handled"
            src, gname = n.input[0], n.input[1]
        else:
            assert len(n.input) == 3, "SkipSimplifiedLayerNormalization bias input is not handled"
            assert not any(n.output[1:3]), "mean / inv_std outputs are not handled"
            sum_name = n.output[3] if len(n.output) > 3 and n.output[3] else None
            src = b.op("Add", [n.input[0], n.input[1]], out=sum_name)
            gname = n.input[2]
        assert gname in inits, f"{n.name}: scale {gname} is not an initializer"
        gamma = numpy_helper.to_array(inits[gname]).astype(np.float64).reshape(-1)
        D = gamma.size
        scale = gamma * math.sqrt(D)
        stats["scale_const_overflow_fp16"] += int(np.count_nonzero(np.abs(scale) > 65504.0))
        if np.all(scale == scale[0]):
            carr = np.array(scale[0], dtype=F32)        # uniform scale -> scalar constant
            stats["scalar_scales"] += 1
        else:
            carr = scale.astype(F32)
        c = b.const(carr, F32)
        if mode == "maxabs":
            m = b.op("ReduceMax", [b.op("Abs", [src]), b.const([-1], I64)], keepdims=1)
            u = b.op("Div", [src, m])
        elif mode == "prescale":
            u = b.op("Mul", [src, b.const(1.0 / 64.0, F32)])
        else:
            u = src
        y = b.op("LpNormalization", [u], axis=-1, p=2)
        b.op("Mul", [y, c], out=n.output[0])

    hits = P._rewrite(model, lambda n: n.op_type in NORM_OPS, build)
    return hits, stats


# ----------------------------------------------------------------- pass: gelu_none

def gelu_none(model):
    """Each ai.onnx Gelu stays ONE node, switched to the erf form (approximate='none')."""
    hits = 0
    for n in model.graph.node:
        if n.op_type == "Gelu" and n.domain in ("", "ai.onnx"):
            for a in n.attribute:
                if a.name == "approximate":
                    a.s = b"none"
                    break
            else:
                n.attribute.append(helper.make_attribute("approximate", "none"))
            hits += 1
    return hits


# ----------------------------------------------------------------- pass: merge_const_scale

def _const_scale(n, inits):
    """(data_input, factor) when n is Mul/Div with exactly one initializer operand, else None."""
    if n.op_type not in ("Mul", "Div") or len(n.input) != 2 or n.domain not in ("", "ai.onnx"):
        return None
    a, c = n.input
    if n.op_type == "Mul":
        if c in inits and a not in inits:
            return a, numpy_helper.to_array(inits[c]).astype(np.float64)
        if a in inits and c not in inits:
            return c, numpy_helper.to_array(inits[a]).astype(np.float64)
        return None
    if c in inits and a not in inits:
        return a, 1.0 / numpy_helper.to_array(inits[c]).astype(np.float64)
    return None


def merge_const_scale(model):
    """Mul(Mul(x,c1),c2) -> Mul(x,c1*c2) (and Div by const as Mul by reciprocal), when the inner
    result has one consumer and is not a graph output. Returns the number of merges."""
    g = model.graph
    merged, k = 0, 0
    while True:
        inits = {t.name: t for t in g.initializer if t.data_type == onnx.TensorProto.FLOAT}
        prod = {o: n for n in g.node for o in n.output if o}
        cons = Counter(x for n in g.node for x in n.input if x)
        protected = {o.name for o in g.output}
        changed = False
        for n in g.node:
            outer = _const_scale(n, inits)
            if outer is None:
                continue
            x, f = outer
            p = prod.get(x)
            if p is None or cons[x] != 1 or x in protected:
                continue
            inner = _const_scale(p, inits)
            if inner is None:
                continue
            px, pf = inner
            prod_c = np.asarray(pf * f, dtype=np.float64).astype(F32)
            k += 1
            name = f"groupD/merged_scale/{k}"
            g.initializer.extend([numpy_helper.from_array(prod_c, name)])
            inits[name] = g.initializer[-1]
            n.op_type = "Mul"
            del n.input[:]
            n.input.extend([px, name])
            idx = next(i for i, m in enumerate(g.node) if m is p)
            del g.node[idx]
            merged += 1
            changed = True
            break
        if not changed:
            return merged


# ----------------------------------------------------------------- shared-data writer

class SharedWriter:
    """Writes many ONNX models whose big tensors all live in one data file.

    Byte-identical tensors are stored once across every model written through this writer.
    """

    def __init__(self, data_path, threshold=1024):
        self.loc = os.path.basename(data_path)
        self.f = open(data_path, "wb")
        self.offset = 0
        self.seen = {}
        self.threshold = threshold

    def save(self, model, onnx_path):
        for init in model.graph.initializer:
            raw = P._raw(init)
            if len(raw) < self.threshold:
                continue
            key = hashlib.sha256(raw).hexdigest() + ":" + str(len(raw))
            if key not in self.seen:
                pad = (-self.offset) % 64
                self.f.write(b"\0" * pad)
                self.offset += pad
                self.seen[key] = self.offset
                self.f.write(raw)
                self.offset += len(raw)
            init.raw_data = raw
            for fld in ("float_data", "int32_data", "int64_data", "double_data", "uint64_data"):
                init.ClearField(fld)
            del init.external_data[:]
            set_external_data(init, self.loc, offset=self.seen[key], length=len(raw))
            init.ClearField("raw_data")
        os.makedirs(os.path.dirname(onnx_path), exist_ok=True)
        onnx.save_model(model, onnx_path)

    def close(self):
        self.f.close()
        return self.offset


# ----------------------------------------------------------------- topological order

def topo_sort(model):
    """Stable topological order of graph nodes. The fp16 converter can leave a consumer ahead of its
    producer; groupA dce walks nodes in reverse and would then drop the producer. Returns node count."""
    g = model.graph
    avail = {i.name for i in g.input} | {i.name for i in g.initializer}
    remaining, out = list(g.node), []
    while remaining:
        nxt, progressed = [], False
        for n in remaining:
            if all((not x) or x in avail for x in n.input):
                out.append(n)
                avail.update(o for o in n.output if o)
                progressed = True
            else:
                nxt.append(n)
        if not progressed:
            raise RuntimeError("graph has a cycle or a dangling input: " + nxt[0].name)
        remaining = nxt
    P._set_nodes(g, out)
    return len(out)
