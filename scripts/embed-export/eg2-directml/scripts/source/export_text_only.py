"""Create a text-only Gemma2 ONNX specialization without reading weights.

The source graph's multimodal merge restores token order after concatenating
text, image, video, and audio embeddings. For text-only token IDs (which must
not contain image/video/audio sentinel IDs), that gather is exactly the text
embedding rows in order. This export rewires the final reshape to those rows,
then removes unreachable modality nodes and public inputs. External tensor
data is copied byte-for-byte and remains external to the ONNX protobuf.
"""
import argparse
import json
from pathlib import Path
import shutil

import onnx
import hashlib


def sha256(path):
    """SHA-256 of a file, read in 1 MiB blocks."""
    digest = hashlib.sha256()
    with open(path, "rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


MODALITY_INPUTS = {'image_features', 'video_features', 'audio_features'}
MERGE_ROWS = '/model/multimodal_merge/Reshape_text/output_0'
MERGE_GATHER = '/model/multimodal_merge/Gather/output_0'
FINAL_RESHAPE = '/model/multimodal_merge/Reshape_embeds'
TEXT_ONLY_SENTINEL_IDS = (258880, 258884, 258881)


def specialize(model):
    """Bypass multimodal routing and retain only nodes feeding graph outputs."""
    graph = model.graph
    final_reshape = [node for node in graph.node if node.name == FINAL_RESHAPE]
    if len(final_reshape) != 1 or final_reshape[0].input[0] != MERGE_GATHER:
        raise ValueError('unexpected multimodal merge graph; refusing specialization')
    if not MODALITY_INPUTS.issubset({item.name for item in graph.input}):
        raise ValueError('expected all three modality graph inputs')

    # Preserve the batch/sequence reshape below this edge. Only replace the
    # token-row source: for ordinary text IDs, Gather's indices are [0..N-1].
    final_reshape[0].input[0] = MERGE_ROWS

    live_values = {item.name for item in graph.output}
    live_nodes = []
    producers = {output: node for node in graph.node for output in node.output if output}
    while True:
        pending = list(live_values)
        while pending:
            value = pending.pop()
            node = producers.get(value)
            if node is None or node in live_nodes:
                continue
            live_nodes.append(node)
            pending.extend(name for name in node.input if name)
        break
    retained_nodes = [node for node in graph.node if node in live_nodes]
    del graph.node[:]
    graph.node.extend(retained_nodes)

    live_node_inputs = {name for node in graph.node for name in node.input if name}
    live_node_outputs = {name for node in graph.node for name in node.output if name}
    live_initializers = {item.name for item in graph.initializer if item.name in live_node_inputs}
    retained_initializers = [item for item in graph.initializer if item.name in live_initializers]
    del graph.initializer[:]
    graph.initializer.extend(retained_initializers)
    retained_inputs = [item for item in graph.input if item.name not in MODALITY_INPUTS]
    del graph.input[:]
    graph.input.extend(retained_inputs)
    live_names = (live_node_inputs | live_node_outputs | live_initializers |
                  {item.name for item in graph.input} | {item.name for item in graph.output})
    retained_value_info = [item for item in graph.value_info if item.name in live_names]
    del graph.value_info[:]
    graph.value_info.extend(retained_value_info)
    retained_annotations = [item for item in graph.quantization_annotation
                            if item.tensor_name in live_names]
    del graph.quantization_annotation[:]
    graph.quantization_annotation.extend(retained_annotations)
    return model


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--source-dir', type=Path, required=True,
                        help='Pinned model snapshot root containing onnx/model.onnx')
    parser.add_argument('--output-dir', type=Path, required=True)
    args = parser.parse_args()
    source_graph = args.source_dir / 'onnx' / 'model.onnx'
    source_data = source_graph.with_name('model.onnx_data')
    output_graph = args.output_dir / 'onnx' / 'model_text.onnx'
    output_data = output_graph.with_name(output_graph.name + '_data')
    if output_graph.exists() or output_data.exists():
        raise FileExistsError(f'refusing to overwrite text-only export: {output_graph}')

    # External initializers stay as references in this structural-only load.
    model = onnx.load(str(source_graph), load_external_data=False)
    source_nodes = len(model.graph.node)
    specialize(model)
    for tensor in model.graph.initializer:
        for entry in tensor.external_data:
            if entry.key == 'location':
                assert entry.value == source_data.name, entry.value
                entry.value = output_data.name
    output_graph.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(source_data, output_data)
    onnx.save_model(model, str(output_graph))
    for filename in ('tokenizer.json', 'tokenizer_config.json', 'config.json',
                     'special_tokens_map.json'):
        source = args.source_dir / filename
        if source.exists():
            shutil.copy2(source, args.output_dir / filename)
    (args.output_dir / 'qualification-input-contract.json').write_text(json.dumps({
        'forbidden_token_ids': list(TEXT_ONLY_SENTINEL_IDS),
        'mode': 'text-only; multimodal input deliberately unsupported',
    }, indent=2), encoding='utf-8')
    (args.output_dir / 'export-receipt.json').write_text(json.dumps({
        'source_graph_sha256': sha256(source_graph), 'source_weights_sha256': sha256(source_data),
        'graph_sha256': sha256(output_graph), 'external_weights_sha256': sha256(output_data),
        'source_nodes': source_nodes, 'text_nodes': len(model.graph.node),
        'source_operator_domains_preserved': True, 'activations': 'FP32',
        'equivalence_scope': 'text token IDs excluding modality sentinels',
    }, indent=2), encoding='utf-8')
    print(f'Wrote {output_graph}; accepted text IDs must exclude {TEXT_ONLY_SENTINEL_IDS}')


if __name__ == '__main__':
    main()
