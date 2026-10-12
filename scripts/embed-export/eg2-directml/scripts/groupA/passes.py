"""Composable ONNX passes for the EmbeddingGemma 2 text graph (Group A).

Every pass mutates the ModelProto it receives and returns a count (or a small
result). Passes are independent except: `embeds` expects the source token
Gather, and `static` must run last (it returns a new model per length).

  gelu(model)            ai.onnx Gelu (approximate none|tanh) -> Mul/Add/Tanh or Erf/Mul
  rope(model)            com.microsoft RotaryEmbedding (interleaved=0) -> Shape/Gather/Slice/Mul/Concat
  rmsnorm(model)         SimplifiedLayerNormalization and SkipSimplifiedLayerNormalization
                         -> x * scale / sqrt(mean(x^2) + eps)
  stable_norm(model)     same, but normalised by the row max first (never squares a large value)
  embeds(model, side)    drop the token Gather + table; add float32 input inputs_embeds [b,s,512];
                         table is returned in side["embed_table"] (no scaling is in the graph)
  static(model, length)  -> (new model, folded_count) with fixed [1,length] shapes and the
                         shape-only subgraph constant-folded (ReferenceEvaluator, input-free nodes)
  cse(model), dce(model), drop_unused_domains(model), save_shared(items, data_path)
"""

import copy
import hashlib
import math
import os

import numpy as np
import onnx
from onnx import TensorProto, helper, numpy_helper, shape_inference
from onnx.external_data_helper import set_external_data
from onnx.reference import ReferenceEvaluator

OPSET = 21
CONTRIB = "com.microsoft"
F32 = np.float32
I64 = np.int64
MAX_FOLD_ELEMS = 1_000_000
EMBED_DIM = 512
HIDDEN = 768


def attr(node, name, default=None):
    for a in node.attribute:
        if a.name == name:
            return helper.get_attribute_value(a)
    return default


# ----------------------------------------------------------------- graph plumbing

def _set_nodes(g, nodes):
    """Replace the node list with clones (protobuf cannot re-insert owned messages)."""
    copies = []
    for n in nodes:
        c = onnx.NodeProto()
        c.CopyFrom(n)
        copies.append(c)
    del g.node[:]
    g.node.extend(copies)


class _Names:
    def __init__(self, model):
        g = model.graph
        self.taken = set()
        for n in g.node:
            self.taken.update(n.input)
            self.taken.update(n.output)
            if n.name:
                self.taken.add(n.name)
        self.taken.update(i.name for i in g.initializer)
        self.taken.update(v.name for v in list(g.input) + list(g.output))
        self.k = 0
        self.inits = []
        self.consts = {}

    def fresh(self, hint):
        while True:
            self.k += 1
            s = f"alc/{hint}/{self.k}"
            if s not in self.taken:
                self.taken.add(s)
                return s


class _Block:
    """Builds the replacement nodes for one rewritten node."""

    def __init__(self, names, prefix):
        self.names = names
        self.prefix = prefix
        self.nodes = []

    def const(self, value, dtype):
        arr = np.asarray(value, dtype=dtype)
        key = (arr.dtype.str, arr.shape, arr.tobytes())
        if key not in self.names.consts:
            name = self.names.fresh("const")
            self.names.inits.append(numpy_helper.from_array(arr, name))
            self.names.consts[key] = name
        return self.names.consts[key]

    def op(self, op_type, inputs, out=None, **attrs):
        out = out or self.names.fresh(op_type.lower())
        node = helper.make_node(
            op_type, list(inputs), [out],
            name=self.names.fresh(f"{self.prefix}/{op_type}"), **attrs)
        self.nodes.append(node)
        return out


def _rewrite(model, match, build):
    g = model.graph
    names = _Names(model)
    out, hits = [], 0
    for n in g.node:
        if match(n):
            blk = _Block(names, (n.name or n.output[0]).replace("/", "_"))
            build(n, blk)
            assert blk.nodes[-1].output[0] == n.output[0], (n.name, blk.nodes[-1].output)
            out.extend(blk.nodes)
            hits += 1
        else:
            out.append(n)
    _set_nodes(g, out)
    g.initializer.extend(names.inits)
    return hits


# ----------------------------------------------------------------- pass: gelu

def gelu(model):
    """Gelu -> primitives.
    tanh: 0.5*x*(1+tanh(sqrt(2/pi)*(x+0.044715*x^3)))   (the source graph uses this form)
    none: 0.5*x*(1+erf(x/sqrt(2)))
    """

    def build(n, b):
        x, y = n.input[0], n.output[0]
        approx = attr(n, "approximate", "none")
        if isinstance(approx, bytes):
            approx = approx.decode()
        if approx == "tanh":
            x2 = b.op("Mul", [x, x])
            x3 = b.op("Mul", [x2, x])
            t = b.op("Add", [x, b.op("Mul", [x3, b.const(0.044715, F32)])])
            t = b.op("Tanh", [b.op("Mul", [t, b.const(math.sqrt(2.0 / math.pi), F32)])])
        elif approx == "none":
            t = b.op("Erf", [b.op("Mul", [x, b.const(1.0 / math.sqrt(2.0), F32)])])
        else:
            raise ValueError(f"unsupported Gelu approximate={approx}")
        h = b.op("Mul", [x, b.const(0.5, F32)])
        b.op("Mul", [h, b.op("Add", [t, b.const(1.0, F32)])], out=y)

    return _rewrite(model, lambda n: n.op_type == "Gelu" and n.domain in ("", "ai.onnx"), build)


# ----------------------------------------------------------------- pass: rope

def rope(model):
    """com.microsoft RotaryEmbedding (non-interleaved, rotary dim = full head) -> standard ops.

    Inputs: x [b,s,H*D] (3-D; H inferred by the contrib op), position_ids [1] (sequence
    offset), cos_cache [S,D/2], sin_cache [S,D/2]. D is taken from the cache width (2*W),
    so the same subgraph serves sliding (D=256) and full (D=512) attention layers.
    rotate-half: o1 = x1*cos - x2*sin, o2 = x2*cos + x1*sin, x1/x2 = first/second half.
    """

    def build(n, b):
        if attr(n, "interleaved", 0) or attr(n, "num_heads", 0) or attr(n, "rotary_embedding_dim", 0):
            raise NotImplementedError("only non-interleaved, full-dim RotaryEmbedding is handled")
        x, pos, cos_c, sin_c = n.input[:4]
        y = n.output[0]
        xs = b.op("Shape", [x])                                   # [b,s,hidden]
        s = b.op("Gather", [xs, b.const(1, I64)])                 # scalar seq length
        half = b.op("Gather", [b.op("Shape", [cos_c]), b.const([-1], I64)])   # [1] = D/2
        full = b.op("Mul", [half, b.const([2], I64)])             # [1] = D
        target = b.op("Concat", [b.const([0, 0, -1], I64), full], axis=0)
        x4 = b.op("Reshape", [x, target])                         # [b,s,H,D]
        ax3 = b.const([3], I64)
        x1 = b.op("Slice", [x4, b.const([0], I64), half, ax3])
        x2 = b.op("Slice", [x4, half, full, ax3])
        r = b.op("Range", [b.const(0, I64), s, b.const(1, I64)])
        idx = b.op("Add", [r, b.op("Gather", [pos, b.const(0, I64)])])
        ax02 = b.const([0, 2], I64)
        cb = b.op("Unsqueeze", [b.op("Gather", [cos_c, idx], axis=0), ax02])   # [1,s,1,D/2]
        sb = b.op("Unsqueeze", [b.op("Gather", [sin_c, idx], axis=0), ax02])
        o1 = b.op("Sub", [b.op("Mul", [x1, cb]), b.op("Mul", [x2, sb])])
        o2 = b.op("Add", [b.op("Mul", [x2, cb]), b.op("Mul", [x1, sb])])
        o = b.op("Concat", [o1, o2], axis=3)
        b.op("Reshape", [o, xs], out=y)

    return _rewrite(model, lambda n: n.op_type == "RotaryEmbedding" and n.domain == CONTRIB, build)


# ----------------------------------------------------------------- pass: rmsnorm / stable_norm

_NORM_OPS = ("SimplifiedLayerNormalization", "SkipSimplifiedLayerNormalization")


def _rms_core(b, x, scale, eps, out, stable):
    axes = b.const([-1], I64)
    if not stable:
        ms = b.op("ReduceMean", [b.op("Mul", [x, x]), axes], keepdims=1)
        rs = b.op("Sqrt", [b.op("Add", [ms, b.const(eps, F32)])])
        y = b.op("Div", [x, rs])
    else:
        # x = m*u with m = max|x| per row; x/rms(x) = u / sqrt(mean(u^2) + eps/m^2) exactly,
        # for any positive m. m is floored at 1e-18 so m^2 neither underflows nor overflows.
        m = b.op("Max", [b.op("ReduceMax", [b.op("Abs", [x]), axes], keepdims=1), b.const(1e-18, F32)])
        u = b.op("Div", [x, m])
        ms = b.op("ReduceMean", [b.op("Mul", [u, u]), axes], keepdims=1)
        e = b.op("Div", [b.const(eps, F32), b.op("Mul", [m, m])])
        rs = b.op("Sqrt", [b.op("Add", [ms, e])])
        y = b.op("Div", [u, rs])
    b.op("Mul", [y, scale], out=out)


def _norm_pass(model, stable):
    def build(n, b):
        eps = float(attr(n, "epsilon", 1e-5))
        if n.op_type == "SimplifiedLayerNormalization":
            assert int(attr(n, "axis", -1)) == -1, "only last-axis norm is handled"
            assert not any(n.output[1:]), "auxiliary inv_std_var output is not handled"
            src, scale = n.input[0], n.input[1]
        else:
            assert len(n.input) == 3, "SkipSimplifiedLayerNormalization bias input is not handled"
            assert not any(n.output[1:3]), "mean / inv_std outputs are not handled"
            sum_name = n.output[3] if len(n.output) > 3 and n.output[3] else None
            src = b.op("Add", [n.input[0], n.input[1]], out=sum_name)   # residual sum (output 3)
            scale = n.input[2]
        _rms_core(b, src, scale, eps, n.output[0], stable)

    return _rewrite(model, lambda n: n.op_type in _NORM_OPS, build)


def rmsnorm(model):
    """SimplifiedLayerNormalization / SkipSimplifiedLayerNormalization -> standard ops."""
    return _norm_pass(model, stable=False)


def stable_norm(model):
    """As rmsnorm, but divides by the row max|x| first so no large activation is squared."""
    return _norm_pass(model, stable=True)


# ----------------------------------------------------------------- pass: embeds

def embeds(model, side):
    """Move the token lookup out of the graph.

    Removes Gather(model.embed_tokens.weight, input_ids) and the table initializer, and
    renames input_ids -> inputs_embeds [batch_size, sequence_length, 512] float32.
    The Reshape/Shape/Concat chain that rebuilt [b,s,512] from the ids is reduced to
    Shape(inputs_embeds). Graph has no scaling after the lookup, so the caller passes
    table rows unscaled.
    """
    g = model.graph
    gi = [i for i, n in enumerate(g.node) if n.op_type == "Gather"
          and list(n.input[:2]) == ["model.embed_tokens.weight", "input_ids"]]
    assert len(gi) == 1, gi
    gn = g.node[gi[0]]
    table_name = gn.input[0]
    ti = [i for i, t in enumerate(g.initializer) if t.name == table_name]
    assert len(ti) == 1
    table = numpy_helper.to_array(g.initializer[ti[0]])
    assert table.dtype == F32 and table.shape == (262144, EMBED_DIM), (table.dtype, table.shape)
    side["embed_table"] = table

    ci = [i for i, n in enumerate(g.node) if n.name == "/model/multimodal_merge/Concat_shape"]
    assert len(ci) == 1
    cn = g.node[ci[0]]
    assert cn.op_type == "Concat" and "/model/multimodal_merge/Shape_ids" in cn.input[0]
    rename = {gn.output[0]: "inputs_embeds", "input_ids": "inputs_embeds", cn.output[0]: cn.input[0]}

    for idx in sorted([gi[0], ci[0]], reverse=True):
        del g.node[idx]
    del g.initializer[ti[0]]
    for n in g.node:
        for k, x in enumerate(n.input):
            if x in rename:
                n.input[k] = rename[x]

    new_in = helper.make_tensor_value_info("inputs_embeds", TensorProto.FLOAT,
                                           ["batch_size", "sequence_length", EMBED_DIM])
    rest = []
    for vi in g.input:
        if vi.name == "input_ids":
            continue
        c = onnx.ValueInfoProto()
        c.CopyFrom(vi)
        rest.append(c)
    del g.input[:]
    g.input.extend([new_in] + rest)

    for n in g.node:
        assert "input_ids" not in n.input and table_name not in n.input, n.name
    # Source value_info still says Shape(input_ids) has rank 2; drop all stale shape hints.
    del g.value_info[:]
    return 1


# ----------------------------------------------------------------- generic cleanup

def cse(model):
    """Merge identical nodes (same op, domain, inputs, attributes). Returns nodes removed."""
    g = model.graph
    protected = {o.name for o in g.output}
    seen, rename, kept = {}, {}, []
    before = len(g.node)
    for n in g.node:
        ins = [rename.get(x, x) for x in n.input]
        if list(n.input) != ins:
            del n.input[:]
            n.input.extend(ins)
        key = (n.op_type, n.domain, tuple(ins),
               tuple(sorted((a.name, a.SerializeToString()) for a in n.attribute)), len(n.output))
        prev = seen.get(key)
        if prev is not None and not any(o in protected for o in n.output if o):
            for o, p in zip(n.output, prev.output):
                if o:
                    rename[o] = p
            continue
        seen.setdefault(key, n)
        kept.append(n)
    _set_nodes(g, kept)
    return before - len(g.node)


def dce(model):
    """Remove nodes and initializers that do not reach a graph output."""
    g = model.graph
    needed = {o.name for o in g.output}
    keep = [False] * len(g.node)
    for i in range(len(g.node) - 1, -1, -1):
        n = g.node[i]
        if any(o and o in needed for o in n.output):
            keep[i] = True
            needed.update(x for x in n.input if x)
    _set_nodes(g, [n for n, k in zip(list(g.node), keep) if k])
    for i in reversed(range(len(g.initializer))):
        if g.initializer[i].name not in needed:
            del g.initializer[i]


def drop_unused_domains(model):
    """Drop the com.microsoft opset import when no node uses it. Returns True if dropped."""
    if any(n.domain == CONTRIB for n in model.graph.node):
        return False
    for i in reversed(range(len(model.opset_import))):
        if model.opset_import[i].domain == CONTRIB:
            del model.opset_import[i]
    return True


# ----------------------------------------------------------------- pass: static

def _dims(vi):
    t = vi.type.tensor_type
    if not t.HasField("shape"):
        return None
    out = []
    for d in t.shape.dim:
        if not d.HasField("dim_value"):
            return None
        out.append(d.dim_value)
    return out


def _set_dims(vi, dims):
    shape = vi.type.tensor_type.shape
    del shape.dim[:]
    for d in dims:
        shape.dim.add().dim_value = d


def _fold_eval(n, ins, shp):
    """Evaluate one input-free node with onnx.reference. Returns arrays or None."""
    if n.domain not in ("", "ai.onnx"):
        return None
    inits, seen = [], set()
    for x, a in zip(n.input, ins):
        if x not in seen:
            seen.add(x)
            inits.append(numpy_helper.from_array(np.asarray(a), x))
    outs = []
    for o in n.output:
        if not o:
            continue
        dims, et = shp.get(o, (None, 0))
        if not et:
            return None
        outs.append(helper.make_tensor_value_info(o, et, dims))
    node = onnx.NodeProto()
    node.CopyFrom(n)
    graph = helper.make_graph([node], "fold", [], outs, initializer=inits)
    model = helper.make_model(graph, opset_imports=[helper.make_opsetid("", OPSET)])
    try:
        res = ReferenceEvaluator(model).run(None, {})
    except Exception:
        return None
    res = [np.asarray(r) for r in res]
    if any(r.size > MAX_FOLD_ELEMS for r in res):
        return None
    return res


def static(model, length):
    """Fixed-shape copy for batch 1 and sequence `length`. Returns (model, folded_nodes)."""
    m = copy.deepcopy(model)
    g = m.graph
    for vi in g.input:
        if vi.name == "inputs_embeds":
            _set_dims(vi, [1, length, EMBED_DIM])
        elif vi.name in ("input_ids", "attention_mask"):
            _set_dims(vi, [1, length])
    for vi in g.output:
        _set_dims(vi, [1, length, HIDDEN] if vi.name == "last_hidden_state" else [1, HIDDEN])
    del g.value_info[:]
    m = shape_inference.infer_shapes(m, data_prop=True)
    g = m.graph

    shp = {}
    for vi in list(g.input) + list(g.value_info) + list(g.output):
        shp[vi.name] = (_dims(vi), vi.type.tensor_type.elem_type)

    inits = {t.name: t for t in g.initializer}
    vals = {}

    def val(name):
        if name in vals:
            return vals[name]
        if name in inits and int(np.prod(inits[name].dims or [1])) <= MAX_FOLD_ELEMS:
            vals[name] = numpy_helper.to_array(inits[name])
            return vals[name]
        return None

    kept, produced, folded = [], {}, 0
    for n in g.node:
        res = None
        if n.op_type == "Shape" and n.domain in ("", "ai.onnx") and shp.get(n.input[0], (None,))[0]:
            dims = shp[n.input[0]][0]
            s, e = attr(n, "start", 0), attr(n, "end", len(dims))
            res = [np.array(dims[s:e], dtype=I64)]
        elif n.input and all(n.input):
            ins = [val(x) for x in n.input]
            if all(v is not None for v in ins):
                res = _fold_eval(n, ins, shp)
        if res is None:
            kept.append(n)
            continue
        names = [o for o in n.output if o]
        for name, arr in zip(names, res):
            vals[name] = arr
            produced[name] = arr
        folded += 1
    _set_nodes(g, kept)
    for name, arr in produced.items():
        if name not in inits:
            g.initializer.extend([numpy_helper.from_array(arr, name)])
    dce(m)
    return m, folded


# ----------------------------------------------------------------- writer

def _raw(init):
    if init.data_location == TensorProto.EXTERNAL and not init.raw_data:
        raise RuntimeError(f"{init.name}: external data not loaded")
    if init.raw_data:
        return bytes(init.raw_data)
    return numpy_helper.to_array(init).tobytes()


def save_shared(items, data_path, threshold=1024):
    """Write (model, onnx_path) pairs; every initializer >= threshold bytes goes into the single
    `data_path` file. Byte-identical tensors are stored once, so several models can share it.
    Returns the data file size in bytes."""
    loc = os.path.basename(data_path)
    seen, offset = {}, 0
    with open(data_path, "wb") as f:
        for model, path in items:
            for init in model.graph.initializer:
                raw = _raw(init)
                if len(raw) < threshold:
                    continue
                key = hashlib.sha256(raw).hexdigest() + ":" + str(len(raw))
                if key not in seen:
                    pad = (-offset) % 64
                    f.write(b"\0" * pad)
                    offset += pad
                    seen[key] = offset
                    f.write(raw)
                    offset += len(raw)
                # set_external_data needs raw_data present (typed-field tensors included);
                # the bytes are dropped right after, so the model keeps only the pointer.
                init.raw_data = raw
                for fld in ("float_data", "int32_data", "int64_data", "double_data", "uint64_data"):
                    init.ClearField(fld)
                del init.external_data[:]
                set_external_data(init, loc, offset=seen[key], length=len(raw))
                init.ClearField("raw_data")
            os.makedirs(os.path.dirname(path), exist_ok=True)
            onnx.save_model(model, path)
    return offset


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 22), b""):
            h.update(chunk)
    return h.hexdigest()
