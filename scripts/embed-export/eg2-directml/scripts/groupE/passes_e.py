"""Group E passes: fewer graph nodes on top of group D (scripts\\groupD\\passes_d.py).

Reuses groupD passes_d (lp_norm, gelu_none, merge_const_scale, _const_scale, topo_sort) and groupA
passes (rope, embeds, static, cse, dce, drop_unused_domains, attr, _set_nodes) unchanged.

  fold_scales(m)      Mul/Div by a constant scalar or last-axis vector next to a constant-weight MatMul is
                      folded into the weight, in fp32:
                        before : MatMul(Mul(x,c), W)  -> MatMul(x, diag(c) W)      (W is [D,out])
                        after  : Mul(MatMul(x,W), c)  -> MatMul(x, W diag(c))      (W is [D,out])
                      A weight shared by several consumers is duplicated per consumer.
  rope_compact(m)     Static-length model only (after static()). Each rotate-half RoPE subgraph
                      (Reshape, 2 Slice, 4 Mul, Sub, Add, Concat, Reshape) becomes
                      out = Add(Mul(x, COS), Mul(Gather(x, PERM, axis=-1), SIN)), 4 nodes.
                      COS, SIN, PERM are precomputed for the static length and head count, deduplicated
                      by content, and shared across layers.
  clean(m)            CSE + DCE + topo_sort, then noop_pairs, repeated to a fixed point.
  noop_pairs(m)       Removes provably redundant nodes: Identity; Reshape to the same static shape;
                      Reshape(Reshape(x, s1), s2) -> Reshape(x, s2) when s2 is a constant without 0;
                      Cast to the input's own dtype; Cast(Cast(x, A), B) with B == dtype(x) when the
                      round trip through A is exact (A wider than B).
"""
import collections
from collections import Counter

import numpy as np
import onnx
from onnx import TensorProto, helper, numpy_helper, shape_inference

import passes_d as D
import passes as P

F32 = np.float32
I64 = np.int64
TYPE_EXACT_ROUNDTRIP = {  # (narrow B, wide A): x:B -> A -> B is exact
    (TensorProto.FLOAT16, TensorProto.FLOAT), (TensorProto.FLOAT16, TensorProto.DOUBLE),
    (TensorProto.FLOAT, TensorProto.DOUBLE), (TensorProto.INT32, TensorProto.INT64),
}


def _users(g):
    prod, cons = {}, collections.defaultdict(list)
    for n in g.node:
        for o in n.output:
            if o:
                prod[o] = n
        for x in n.input:
            if x:
                cons[x].append(n)
    return prod, cons


def _is_matmul(n):
    return n.op_type == "MatMul" and n.domain in ("", "ai.onnx") and len(n.input) == 2


def _weight(n, tmap):
    """The 2-D float initializer that is the second operand of MatMul n, else None."""
    if not _is_matmul(n) or n.input[1] not in tmap:
        return None
    t = tmap[n.input[1]]
    return t if len(t.dims) == 2 else None


def fold_scales(model):
    """Fold constant scale Mul/Div into adjacent constant-weight MatMul. Returns a stats dict."""
    g = model.graph
    protected = {o.name for o in g.output}
    stats = Counter()
    k = 0
    while True:
        tmap = {t.name: t for t in g.initializer if t.data_type == TensorProto.FLOAT}
        prod, cons = _users(g)
        hit = None
        for idx, n in enumerate(g.node):
            cs = D._const_scale(n, tmap)
            if cs is None:
                continue
            x, f = cs
            y = n.output[0]
            if not y or y in protected:
                continue
            fa = np.asarray(f, np.float64)
            if fa.size != 1 and (fa.ndim == 0 or fa.shape[-1] != fa.size):
                continue  # not a per-last-axis vector: would broadcast the shape
            fv = fa.reshape(-1)
            users = cons.get(y, [])
            if users and all(u.input[0] == y and _weight(u, tmap) is not None
                             and (fv.size == 1 or fv.size == _weight(u, tmap).dims[0]) for u in users):
                hit = ("before", idx, x, fv, users)
                break
            p = prod.get(x)
            if (p is not None and _is_matmul(p) and len(cons.get(x, [])) == 1 and x not in protected
                    and _weight(p, tmap) is not None and (fv.size == 1 or fv.size == _weight(p, tmap).dims[1])):
                hit = ("after", idx, p, fv, None)
                break
        if hit is None:
            break
        kind, idx, obj, fv, users = hit
        if kind == "before":
            new = []
            for u in users:
                W = numpy_helper.to_array(tmap[u.input[1]]).astype(np.float64)
                new.append(W * (fv[0] if fv.size == 1 else fv[:, None]))
            for u, W2 in zip(users, new):
                k += 1
                name = f"groupE/fold/{k}"
                g.initializer.extend([numpy_helper.from_array(W2.astype(F32), name)])
                u.input[0] = g.node[idx].input[0]
                u.input[1] = name
                stats["weights_rewritten"] += 1
            stats["mul_before_matmul"] += 1
        else:
            p = obj
            W = numpy_helper.to_array(tmap[p.input[1]]).astype(np.float64)
            W2 = W * (fv[0] if fv.size == 1 else fv[None, :])
            k += 1
            name = f"groupE/fold/{k}"
            g.initializer.extend([numpy_helper.from_array(W2.astype(F32), name)])
            y = g.node[idx].output[0]
            p.input[1] = name
            p.output[0] = y
            stats["mul_after_matmul"] += 1
            stats["weights_rewritten"] += 1
        del g.node[idx]
    stats["muls_removed"] = stats["mul_before_matmul"] + stats["mul_after_matmul"]
    return dict(stats)


def shape_map(m):
    """name -> (dims or None, elem_type) from shape inference on a copy without tensor payloads."""
    s = onnx.ModelProto()
    s.ir_version = m.ir_version
    s.opset_import.extend(m.opset_import)
    gr = s.graph
    gr.name = "shape_probe"
    gr.node.extend(m.graph.node)
    gr.input.extend(m.graph.input)
    gr.output.extend(m.graph.output)
    for t in m.graph.initializer:
        # Small values are kept: ops such as Split read their split sizes from an input tensor.
        n_el = int(np.prod(t.dims)) if len(t.dims) else 1
        if n_el <= 4096:
            gr.initializer.append(t)
        else:
            gr.initializer.append(TensorProto(name=t.name, data_type=t.data_type, dims=t.dims))
    inf = shape_inference.infer_shapes(s)
    out = {}
    for vi in list(inf.graph.input) + list(inf.graph.value_info) + list(inf.graph.output):
        t = vi.type.tensor_type
        dims = None
        if t.HasField("shape"):
            dims = [d.dim_value if d.HasField("dim_value") else None for d in t.shape.dim]
        out[vi.name] = (dims, t.elem_type)
    for t in m.graph.initializer:
        out.setdefault(t.name, (list(t.dims), t.data_type))
    return out


def noop_pairs(m):
    """Remove provably redundant Identity / same-shape Reshape / Reshape-chain / Cast nodes."""
    g = m.graph
    try:
        shp = shape_map(m)
    except Exception:
        shp = {}
    protected = {o.name for o in g.output}
    inits = {t.name: t for t in g.initializer}
    prod = {}
    for n in g.node:
        for o in n.output:
            if o:
                prod[o] = n
    rename, drop = {}, set()
    removed = Counter()

    def dims_of(name):
        return shp.get(name, (None, 0))[0]

    def elem_of(name):
        return shp.get(name, (None, 0))[1]

    def static_dims(name):
        d = dims_of(name)
        return d is not None and all(v is not None for v in d)

    for n in g.node:
        if n.domain not in ("", "ai.onnx"):
            continue
        if n.op_type == "Identity" and n.output[0] not in protected:
            rename[n.output[0]] = n.input[0]
            drop.add(id(n))
            removed["identity"] += 1
        elif n.op_type == "Reshape" and n.output[0] not in protected and len(n.input) == 2:
            x, out = n.input[0], n.output[0]
            if static_dims(x) and static_dims(out) and dims_of(x) == dims_of(out):
                rename[out] = x
                drop.add(id(n))
                removed["reshape_same_shape"] += 1
                continue
            pr = prod.get(x)
            s2 = inits.get(n.input[1])
            if (pr is not None and pr.op_type == "Reshape" and pr.domain in ("", "ai.onnx")
                    and s2 is not None and s2.data_type == TensorProto.INT64):
                vals = numpy_helper.to_array(s2).reshape(-1)
                if vals.size and not np.any(vals == 0):
                    n.input[0] = pr.input[0]
                    removed["reshape_chain"] += 1
        elif n.op_type == "Cast" and n.output[0] not in protected:
            to = int(P.attr(n, "to", 0))
            x = n.input[0]
            if to and elem_of(x) == to:
                rename[n.output[0]] = x
                drop.add(id(n))
                removed["cast_same_dtype"] += 1
                continue
            pr = prod.get(x)
            if (pr is not None and pr.op_type == "Cast" and pr.domain in ("", "ai.onnx")
                    and to and elem_of(pr.input[0]) == to
                    and (to, int(P.attr(pr, "to", 0))) in TYPE_EXACT_ROUNDTRIP):
                rename[n.output[0]] = pr.input[0]
                drop.add(id(n))
                removed["cast_roundtrip"] += 1

    def resolve(name):
        seen = 0
        while name in rename and seen < 1000:
            name = rename[name]
            seen += 1
        return name

    if not drop and not removed:
        return 0
    kept = []
    for n in g.node:
        if id(n) in drop:
            continue
        for i, x in enumerate(n.input):
            if x in rename:
                n.input[i] = resolve(x)
        kept.append(n)
    P._set_nodes(g, kept)
    return sum(removed.values())


def clean(m):
    """CSE, DCE and topo order, with noop_pairs, repeated to a fixed point."""
    stats = Counter()
    for _ in range(4):
        D.topo_sort(m)
        c = P.cse(m)
        P.dce(m)
        r = noop_pairs(m)
        stats["cse"] += c
        stats["noop"] += r
        if c == 0 and r == 0:
            break
    D.topo_sort(m)
    return dict(stats)


def rope_compact(m):
    """Replace each rotate-half RoPE subgraph of a static-length model by 4 nodes.
    Returns (matched, unmatched_count)."""
    g = m.graph
    shapes = shape_map(m)
    tmap = {t.name: t for t in g.initializer}
    prod, _ = _users(g)
    consts, k = {}, 0
    replace = {}
    matched, unmatched = 0, 0

    def const(arr, dtype):
        nonlocal k
        arr = np.ascontiguousarray(arr, dtype=dtype)
        key = (arr.dtype.str, arr.shape, arr.tobytes())
        if key not in consts:
            k += 1
            name = f"groupE/rope4/c{k}"
            g.initializer.extend([numpy_helper.from_array(arr, name)])
            consts[key] = name
        return consts[key]

    def mul_parts(n):
        if n is None or n.op_type != "Mul" or len(n.input) != 2:
            return None
        a, b = n.input
        if b in tmap and a not in tmap:
            return a, b
        if a in tmap and b not in tmap:
            return b, a
        return None

    def vals(name):
        return numpy_helper.to_array(tmap[name]).reshape(-1) if name in tmap else None

    for idx, R in enumerate(list(g.node)):
        if R.op_type != "Reshape" or R.domain not in ("", "ai.onnx"):
            continue
        oc = prod.get(R.input[0])
        if oc is None or oc.op_type != "Concat" or len(oc.input) != 2 or int(P.attr(oc, "axis", 0)) != 3:
            continue
        o1, o2 = prod.get(oc.input[0]), prod.get(oc.input[1])
        if o1 is None or o2 is None or o1.op_type != "Sub" or o2.op_type != "Add":
            continue
        pa, pb = mul_parts(prod.get(o1.input[0])), mul_parts(prod.get(o1.input[1]))
        qa, qb = mul_parts(prod.get(o2.input[0])), mul_parts(prod.get(o2.input[1]))
        if not (pa and pb and qa and qb):
            unmatched += 1
            continue
        x1, cb = pa
        x2, sb = pb
        if not (qa[1] == cb and qa[0] == x2 and qb[1] == sb and qb[0] == x1):
            unmatched += 1
            continue
        s1, s2 = prod.get(x1), prod.get(x2)
        if s1 is None or s2 is None or s1.op_type != "Slice" or s2.op_type != "Slice":
            unmatched += 1
            continue
        x4 = s1.input[0]
        if s2.input[0] != x4 or prod.get(x4) is None or prod[x4].op_type != "Reshape":
            unmatched += 1
            continue
        xin = prod[x4].input[0]
        st1, en1, ax1 = vals(s1.input[1]), vals(s1.input[2]), vals(s1.input[3])
        st2, en2, ax2 = vals(s2.input[1]), vals(s2.input[2]), vals(s2.input[3])
        cbv = numpy_helper.to_array(tmap[cb]) if cb in tmap else None
        sbv = numpy_helper.to_array(tmap[sb]) if sb in tmap else None
        xs = shapes.get(xin, (None, 0))[0]
        if any(v is None for v in (st1, en1, ax1, st2, en2, ax2, cbv, sbv)) or xs is None or len(xs) != 3:
            unmatched += 1
            continue
        half = cbv.shape[-1]
        D_ = 2 * half
        L, W = xs[1], xs[2]
        if not all(isinstance(v, int) for v in (L, W)) or W % D_ or cbv.size != L * half or sbv.size != L * half:
            unmatched += 1
            continue
        if not (int(st1[0]) == 0 and int(en1[0]) == half and int(st2[0]) == half and int(en2[0]) == D_
                and int(ax1[0]) == 3 and int(ax2[0]) == 3):
            unmatched += 1
            continue
        H = W // D_
        cb2 = cbv.reshape(L, half).astype(np.float64)
        sb2 = sbv.reshape(L, half).astype(np.float64)
        cos2 = np.concatenate([cb2, cb2], axis=1)
        sin_s = np.concatenate([-sb2, sb2], axis=1)
        COS = np.broadcast_to(cos2[None, :, None, :], (1, L, H, D_)).reshape(1, L, W)
        SIN = np.broadcast_to(sin_s[None, :, None, :], (1, L, H, D_)).reshape(1, L, W)
        perm = np.array([h * D_ + ((d + half) % D_) for h in range(H) for d in range(D_)], dtype=I64)
        cos_name = const(COS, F32)
        sin_name = const(SIN, F32)
        perm_name = const(perm, I64)
        k += 1
        pre = f"groupE/rope4/{k}"
        t1, t2, t3 = f"{pre}/mul_cos", f"{pre}/gather_perm", f"{pre}/mul_sin"
        nodes = [
            helper.make_node("Mul", [xin, cos_name], [t1], name=t1),
            helper.make_node("Gather", [xin, perm_name], [t2], name=t2, axis=-1),
            helper.make_node("Mul", [t2, sin_name], [t3], name=t3),
            helper.make_node("Add", [t1, t3], [R.output[0]], name=f"{pre}/add"),
        ]
        replace[idx] = nodes
        matched += 1
    if not replace:
        return 0, unmatched
    new = []
    for idx, n in enumerate(list(g.node)):
        if idx in replace:
            new.extend(replace[idx])
        else:
            new.append(n)
    P._set_nodes(g, new)
    return matched, unmatched
