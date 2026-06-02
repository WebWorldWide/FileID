#!/usr/bin/env python3
"""Break an ONNX model's initializers down by dtype + report compute-op weight
precision. Answers: is the heavy compute (MatMul/Conv/Gemm) in fp16 or fp32?

Usage: python inspect_onnx.py <model.onnx>
"""
import sys
import onnx
from onnx import numpy_helper

DT = {1: "FLOAT32", 2: "UINT8", 3: "INT8", 6: "INT32", 7: "INT64", 10: "FLOAT16", 11: "DOUBLE", 16: "BFLOAT16"}

path = sys.argv[1]
m = onnx.load(path, load_external_data=False)
g = m.graph

# bytes + count per dtype
by_dt = {}
big = []
for init in g.initializer:
    n = 1
    for d in init.dims:
        n *= d
    elemsize = {1: 4, 10: 2, 11: 8, 6: 4, 7: 8, 3: 1, 2: 1}.get(init.data_type, 4)
    nbytes = n * elemsize
    dt = DT.get(init.data_type, str(init.data_type))
    c, b = by_dt.get(dt, (0, 0))
    by_dt[dt] = (c + 1, b + nbytes)
    big.append((nbytes, init.name, dt, list(init.dims)))

print(f"== {path} ==")
print(f"opset: {[ (i.domain or 'ai.onnx', i.version) for i in m.opset_import ]}")
print(f"initializers: {len(g.initializer)}  nodes: {len(g.node)}")
print("-- initializer bytes by dtype --")
for dt, (c, b) in sorted(by_dt.items(), key=lambda kv: -kv[1][1]):
    print(f"  {dt:<10} count={c:<6} bytes={b/1e6:,.1f} MB")

# dtype of the dtype-carrying inputs to the dominant compute ops
init_dtype = {init.name: DT.get(init.data_type, str(init.data_type)) for init in g.initializer}
op_counts = {}
op_weight_dt = {}
for node in g.node:
    op_counts[node.op_type] = op_counts.get(node.op_type, 0) + 1
    if node.op_type in ("MatMul", "Gemm", "Conv"):
        for inp in node.input:
            if inp in init_dtype:
                key = (node.op_type, init_dtype[inp])
                op_weight_dt[key] = op_weight_dt.get(key, 0) + 1

print("-- top op types --")
for op, c in sorted(op_counts.items(), key=lambda kv: -kv[1])[:12]:
    print(f"  {op:<22} {c}")
print("-- compute-op weight dtypes (MatMul/Gemm/Conv initializer inputs) --")
for (op, dt), c in sorted(op_weight_dt.items(), key=lambda kv: -kv[1]):
    print(f"  {op:<8} weights={dt:<10} x{c}")

print("-- 8 largest initializers --")
for nbytes, name, dt, dims in sorted(big, reverse=True)[:8]:
    print(f"  {nbytes/1e6:8.1f} MB  {dt:<8} {dims}  {name[:50]}")
