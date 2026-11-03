"""Export-only minimal k2 surface for Icefall's encoder construction."""

import torch


class SymbolTable:
    def __init__(self, symbols):
        self.symbols = symbols
        self._ids = {symbol: index for index, symbol in enumerate(symbols)}

    @classmethod
    def from_file(cls, path):
        pairs = []
        with open(path, encoding="utf-8") as source:
            for line in source:
                fields = line.strip().split()
                if len(fields) == 2:
                    pairs.append((fields[0], int(fields[1])))
        size = max(index for _, index in pairs) + 1
        symbols = ["" for _ in range(size)]
        for symbol, index in pairs:
            symbols[index] = symbol
        return cls(symbols)

    def __getitem__(self, symbol):
        return self._ids[symbol]


class RaggedTensor:
    def __init__(self, value):
        self.value = value


with_cuda = False


def swoosh_l_forward(x):
    return torch.logaddexp(torch.zeros_like(x), x - 4.0) - 0.08 * x - 0.035


def swoosh_r_forward(x):
    return torch.logaddexp(torch.zeros_like(x), x - 1.0) - 0.08 * x - 0.313261687


swoosh_l = swoosh_l_forward
swoosh_r = swoosh_r_forward


def __getattr__(name):
    # Icefall imports training/decoding annotations at module import time.
    # Those types are unreachable while constructing this encoder-only export.
    return type(name, (), {})
