"""Group E eps passes: zero-row-safe L2 normalisation for the prescaled RMS norm (E37, E38).

Reuses groupD passes_d (NORM_OPS, lp_norm conventions) and groupA passes (attr, _rewrite, _set_nodes).

  lp_norm_eps(m, mode)  Every SimplifiedLayerNormalization / SkipSimplifiedLayerNormalization becomes
                        (SkipSimplified: residual Add first, as in groupD)
                          u   = x * (1/64)                           Mul, fp32 constant
                          n   = ReduceL2(u, axes=-1, keepdims=1)     the L2 norm of u
                          d   = Add(n, eps_c)   mode "add"  (E37)  or   Max(n, eps_c)  mode "max" (E38)
                          y   = Div(u, d)
                          out = Mul(y, gamma*sqrt(D))                same scale as groupD lp_norm
                        Source form: x / sqrt(mean(x^2) + eps) * gamma. With u = x/64 and n = ||u||_2:
                        x/sqrt(mean(x^2)+eps) = sqrt(D) * u / sqrt(n^2 + eps_c^2), where
                        eps_c = sqrt(D*eps)/64. eps_c is rounded to the nearest fp16 value (exactly
                        representable in fp16, normal range) so the fp16 graph uses the same constant.
                        Node count: LpNormalization (1) -> ReduceL2, Add/Max, Div (3): +2 per norm.
                        A zero row gives u = 0, d = eps_c > 0, out = 0 (finite).
  pool_floor(m)         The sentence-embedding L2 floor Max(ReduceL2(pooled), 1e-12) uses a constant that
                        fp16 flushes to 0. Replace it by 2^-20 (exact in fp16). Same node count.
                        Only the single source Max fed by the single source ReduceL2 is changed; the
                        count is asserted.

The source epsilon is read per norm node (attribute "epsilon"); the (D, eps) -> eps_c table is returned
for the manifest.
"""
import math
import os
import sys
from collections import Counter

import numpy as np
from onnx import helper, numpy_helper

HERE = os.path.dirname(os.path.abspath(__file__))
WORK = os.path.dirname(os.path.dirname(HERE))
sys.path.insert(0, os.path.join(WORK, "scripts", "groupA"))
sys.path.insert(0, os.path.join(WORK, "scripts", "groupD"))

import passes as P  # noqa: E402  (groupA)
import passes_d as D  # noqa: E402  (groupD)

F32 = np.float32
I64 = np.int64
FP16_MIN_NORMAL = 6.103515625e-05
POOL_FLOOR_OLD = 1e-12
POOL_FLOOR_NEW = 2.0 ** -20  # exact in fp16 (subnormal, 16 * 2^-24)


def eps_c_for(dim, eps):
    """(fp16-rounded eps_c, exact value) for a norm of width dim and source epsilon eps."""
    exact = math.sqrt(dim * eps) / 64.0
    rounded = float(np.float16(exact))
    if not (rounded >= FP16_MIN_NORMAL and np.isfinite(rounded)):
        raise ValueError(f"eps_c {exact} for D={dim}, eps={eps} is not a normal fp16 value")
    return rounded, exact


def lp_norm_eps(model, mode):
    """RMS norm with a zero-row-safe L2 normalisation. mode: "add" (E37) or "max" (E38).
    Returns (number of norm nodes rewritten, table) where table maps 'D=..;eps=..' to
    {eps_c_fp16, eps_c_exact, count}."""
    assert mode in ("add", "max"), mode
    g = model.graph
    inits = {t.name: t for t in g.initializer}
    table = {}

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
        dim = gamma.size
        eps_src = float(P.attr(n, "epsilon", 1e-5))
        ec, ec_exact = eps_c_for(dim, eps_src)
        key = f"D={dim};eps={eps_src:.6g}"
        rec = table.setdefault(key, {"eps_c_fp16": ec, "eps_c_exact": ec_exact, "count": 0})
        rec["count"] += 1

        scale = gamma * math.sqrt(dim)
        if np.all(scale == scale[0]):
            carr = np.array(scale[0], dtype=F32)
        else:
            carr = scale.astype(F32)
        c = b.const(carr, F32)
        u = b.op("Mul", [src, b.const(1.0 / 64.0, F32)])
        nrm = b.op("ReduceL2", [u, b.const([-1], I64)], keepdims=1)
        ce = b.const(np.array(ec, dtype=F32), F32)
        d = b.op("Add", [nrm, ce]) if mode == "add" else b.op("Max", [nrm, ce])
        y = b.op("Div", [u, d])
        b.op("Mul", [y, c], out=n.output[0])

    hits = P._rewrite(model, lambda n: n.op_type in D.NORM_OPS, build)
    return hits, table


def pool_floor(model):
    """Replace the sentence-embedding L2 floor 1e-12 (flushed to 0 in fp16) by 2^-20."""
    g = model.graph
    prod = {o: n for n in g.node for o in n.output if o}
    inits = {t.name: t for t in g.initializer}
    hits = []
    for n in g.node:
        if n.op_type != "Max" or len(n.input) != 2:
            continue
        for i in (0, 1):
            x, other = n.input[i], n.input[1 - i]
            t = inits.get(x)
            if t is None or other not in prod:
                continue
            if prod[other].op_type != "ReduceL2":
                continue
            val = numpy_helper.to_array(t).reshape(-1)
            if val.size == 1 and float(val[0]) == float(np.float32(POOL_FLOOR_OLD)):
                hits.append((n.name, x))
    if len(hits) != 1:
        raise SystemExit(f"pool_floor expected exactly one floor constant, found {len(hits)}: {hits}")
    _, name = hits[0]
    idx = next(i for i, t in enumerate(g.initializer) if t.name == name)
    g.initializer[idx].CopyFrom(numpy_helper.from_array(np.array(POOL_FLOOR_NEW, dtype=F32), name))
    return len(hits)


def tail_fp32(m):
    """E40: run the sentence-embedding tail in fp32, on the fp16 graph (after build_one's clean).

    Old tail (fp16 converter output): pooled Div_mean (fp16) -> ReduceL2 -> Cast fp32 -> Max(1e-12, fp32)
    -> Cast fp16 -> Div -> Cast fp32 = sentence_embedding; pooling count: ReduceSum (fp16) -> Cast fp32 ->
    Max(1e-9, fp32) -> Cast fp16.
    New tail: count floor Max(count, 1.0) in fp16 (count is an integer >= 0); mean32 = Cast(mean16, fp32);
    n = ReduceL2(mean32, -1, keepdims) in fp32; nf = Max(n, 1e-12 fp32); sentence_embedding = Div(mean32, nf)
    in fp32 (the graph output is written directly, no final Cast). Unused converter Casts/Max are removed
    by E.clean (DCE). Returns a stats dict with the removed and added node counts.
    """
    g = m.graph
    before = len(g.node)
    prod = {o: n for n in g.node for o in n.output if o}

    def need(name, op):
        n = prod.get(name)
        if n is None or n.op_type != op:
            raise SystemExit(f"tail_fp32: expected {op} producing {name}, got {None if n is None else n.op_type}")
        return n

    out_cast = need("sentence_embedding", "Cast")
    out_div = need(out_cast.input[0], "Div")
    mean16 = out_div.input[0]
    sent_max_cast = need(out_div.input[1], "Cast")
    sent_max = need(sent_max_cast.input[0], "Max")
    rcast = need(sent_max.input[0], "Cast")
    rl2 = need(rcast.input[0], "ReduceL2")
    mean_div = need(mean16, "Div")
    if rl2.input[0] != mean16:
        raise SystemExit("tail_fp32: ReduceL2 is not fed by the pooled mean")
    cnt_cast = need(mean_div.input[1], "Cast")
    cnt_max = need(cnt_cast.input[0], "Max")
    cnt_c32 = need(cnt_max.input[0], "Cast")
    cnt_red = need(cnt_c32.input[0], "ReduceSum")

    one16 = numpy_helper.from_array(np.array(1.0, dtype=np.float16), "groupE/tail32/one16")
    floor32 = numpy_helper.from_array(np.array(1e-12, dtype=F32), "groupE/tail32/floor32")
    g.initializer.extend([one16, floor32])

    cnt_floor = helper.make_node("Max", [cnt_red.output[0], one16.name], ["groupE/tail32/cnt_floor"],
                                 name="groupE/tail32/cnt_max")
    mean_div.input[1] = cnt_floor.output[0]
    mean32 = helper.make_node("Cast", [mean16], ["groupE/tail32/mean32"], name="groupE/tail32/mean_cast",
                              to=1)
    rl2.input[0] = mean32.output[0]
    stale = rl2.output[0]  # old fp16 name: a fresh fp32 name avoids a stale value_info type
    rl2.output[0] = "groupE/tail32/l2"
    keep_vi = [v for v in g.value_info if v.name != stale]
    del g.value_info[:]
    g.value_info.extend(keep_vi)
    nf = helper.make_node("Max", [rl2.output[0], floor32.name], ["groupE/tail32/nfloor"],
                          name="groupE/tail32/norm_max")
    out = helper.make_node("Div", [mean32.output[0], nf.output[0]], ["sentence_embedding"],
                           name="groupE/tail32/div")
    kept = [n for n in g.node if n is not out_cast]
    kept += [cnt_floor, mean32, nf, out]
    # Drop nodes left reading a name that nothing produces any more (the converter's dead Casts on the
    # renamed ReduceL2 output); repeat to a fixed point so the topological sort below sees a clean graph.
    base_names = {i.name for i in g.input} | {i.name for i in g.initializer}
    while True:
        produced = base_names | {o for n in kept for o in n.output if o}
        nxt = [n for n in kept if all((not x) or x in produced for x in n.input)]
        if len(nxt) == len(kept):
            break
        kept = nxt
    P._set_nodes(g, kept)
    import passes_e as E  # noqa: E402  (same folder)
    E.clean(m)
    return {"nodes_before": before, "nodes_after": len(g.node)}
