# lattice-core

Python bindings for [LATTICE](https://github.com/Skyline10124/lattice) — a model-centric LLM engine.

## Installation

```bash
pip install lattice-core
```

Requires Python 3.9+.

## Usage

```python
from lattice_core import LatticeEngine

engine = LatticeEngine()
resolved = engine.resolve("sonnet")
for event in engine.chat(resolved, messages):
    print(event)
```

## License

Apache-2.0