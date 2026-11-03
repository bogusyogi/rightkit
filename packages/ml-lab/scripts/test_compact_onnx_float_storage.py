from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

import numpy as np
import onnx
import onnxruntime as ort
from onnx import TensorProto, helper, numpy_helper

from compact_onnx_float_storage import (
    compact_float_constant_attributes,
    compact_float_initializers,
)


class CompactFloatInitializersTests(unittest.TestCase):
    def test_stores_float_constant_tensor_as_fp16_and_casts_back(self) -> None:
        constant = numpy_helper.from_array(
            np.array([1.125, -2.25], dtype=np.float32), name="constant_value"
        )
        graph = helper.make_graph(
            [
                helper.make_node("Constant", [], ["bias"], name="bias", value=constant),
                helper.make_node("Add", ["input", "bias"], ["output"], name="add"),
            ],
            "tiny_constant",
            [helper.make_tensor_value_info("input", TensorProto.FLOAT, [2])],
            [helper.make_tensor_value_info("output", TensorProto.FLOAT, [2])],
        )
        model = helper.make_model(graph, opset_imports=[helper.make_opsetid("", 17)])
        model.ir_version = 8

        stats = compact_float_constant_attributes(model)

        self.assertEqual(stats["converted"], 1)
        self.assertEqual(model.graph.node[0].op_type, "Constant")
        stored = helper.get_attribute_value(model.graph.node[0].attribute[0])
        self.assertEqual(stored.data_type, TensorProto.FLOAT16)
        self.assertEqual(list(model.graph.node[0].output), ["bias__fp16_storage"])
        self.assertEqual(model.graph.node[1].op_type, "Cast")
        self.assertEqual(list(model.graph.node[1].input), ["bias__fp16_storage"])
        self.assertEqual(list(model.graph.node[1].output), ["bias"])
        onnx.checker.check_model(model)

        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "tiny_constant.onnx"
            onnx.save(model, path)
            session = ort.InferenceSession(str(path), providers=["CPUExecutionProvider"])
            value = np.array([2.0, -1.0], dtype=np.float32)
            actual = session.run(None, {"input": value})[0]
        np.testing.assert_allclose(actual, value + np.array([1.125, -2.25]), rtol=1e-3)

    def test_stores_float_weight_as_fp16_and_casts_back_for_original_node(self) -> None:
        weight = np.array([[1.125, -2.25], [3.5, 4.75]], dtype=np.float32)
        graph = helper.make_graph(
            [helper.make_node("MatMul", ["input", "weight"], ["output"], name="matmul")],
            "tiny",
            [helper.make_tensor_value_info("input", TensorProto.FLOAT, [1, 2])],
            [helper.make_tensor_value_info("output", TensorProto.FLOAT, [1, 2])],
            [numpy_helper.from_array(weight, name="weight")],
        )
        model = helper.make_model(graph, opset_imports=[helper.make_opsetid("", 17)])
        model.ir_version = 8

        stats = compact_float_initializers(model)

        self.assertEqual(stats["converted"], 1)
        self.assertEqual(model.graph.initializer[0].name, "weight__fp16_storage")
        self.assertEqual(model.graph.initializer[0].data_type, TensorProto.FLOAT16)
        self.assertEqual(model.graph.node[0].op_type, "Cast")
        self.assertEqual(list(model.graph.node[0].input), ["weight__fp16_storage"])
        self.assertEqual(list(model.graph.node[0].output), ["weight"])
        self.assertEqual(
            helper.get_attribute_value(model.graph.node[0].attribute[0]),
            TensorProto.FLOAT,
        )
        onnx.checker.check_model(model)

        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "tiny.onnx"
            onnx.save(model, path)
            session = ort.InferenceSession(str(path), providers=["CPUExecutionProvider"])
            value = np.array([[2.0, -1.0]], dtype=np.float32)
            actual = session.run(None, {"input": value})[0]
        expected = value @ weight
        np.testing.assert_allclose(actual, expected, rtol=1e-3, atol=1e-3)

    def test_can_skip_initializers_consumed_by_selected_ops(self) -> None:
        graph = helper.make_graph(
            [helper.make_node("MatMul", ["input", "weight"], ["output"], name="matmul")],
            "tiny",
            [helper.make_tensor_value_info("input", TensorProto.FLOAT, [1, 1])],
            [helper.make_tensor_value_info("output", TensorProto.FLOAT, [1, 1])],
            [numpy_helper.from_array(np.ones((1, 1), dtype=np.float32), name="weight")],
        )
        model = helper.make_model(graph, opset_imports=[helper.make_opsetid("", 17)])

        stats = compact_float_initializers(model, skip_consumer_ops={"MatMul"})

        self.assertEqual(stats["converted"], 0)
        self.assertEqual(model.graph.initializer[0].name, "weight")
        self.assertEqual(model.graph.initializer[0].data_type, TensorProto.FLOAT)

    def test_can_limit_conversion_to_selected_consumer_ops(self) -> None:
        graph = helper.make_graph(
            [helper.make_node("MatMul", ["input", "weight"], ["output"], name="matmul")],
            "tiny",
            [helper.make_tensor_value_info("input", TensorProto.FLOAT, [1, 1])],
            [helper.make_tensor_value_info("output", TensorProto.FLOAT, [1, 1])],
            [numpy_helper.from_array(np.ones((1, 1), dtype=np.float32), name="weight")],
        )
        model = helper.make_model(graph, opset_imports=[helper.make_opsetid("", 17)])

        stats = compact_float_initializers(model, only_consumer_ops={"Conv"})

        self.assertEqual(stats["converted"], 0)
        self.assertEqual(model.graph.initializer[0].data_type, TensorProto.FLOAT)

    def test_can_keep_weights_for_matching_consumer_node_names(self) -> None:
        graph = helper.make_graph(
            [
                helper.make_node(
                    "MatMul", ["input", "weight"], ["output"], name="/layers.0/ffn/MatMul"
                )
            ],
            "tiny",
            [helper.make_tensor_value_info("input", TensorProto.FLOAT, [1, 1])],
            [helper.make_tensor_value_info("output", TensorProto.FLOAT, [1, 1])],
            [numpy_helper.from_array(np.ones((1, 1), dtype=np.float32), name="weight")],
        )
        model = helper.make_model(graph, opset_imports=[helper.make_opsetid("", 17)])

        stats = compact_float_initializers(
            model,
            keep_consumer_name_patterns=[r"/layers\.0/"],
        )

        self.assertEqual(stats["converted"], 0)
        self.assertEqual(model.graph.initializer[0].data_type, TensorProto.FLOAT)


if __name__ == "__main__":
    unittest.main()
