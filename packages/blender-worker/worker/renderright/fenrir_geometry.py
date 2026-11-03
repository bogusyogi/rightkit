"""Compatibility alias: product registration now lives in product_geometry.py.

The Fenrir constants moved to the ``registration`` block of
recipes/samples/fenrir-sample-product.json. This name keeps existing imports resolving.
"""
from .product_geometry import import_and_rest

__all__ = ["import_and_rest"]
