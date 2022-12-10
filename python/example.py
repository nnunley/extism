import sys
import json
import hashlib
import requests

sys.path.append(".")
from extism import Context, Function, host_fn, ValType

if len(sys.argv) > 1:
    data = sys.argv[1].encode()
else:
    data = b"some data from python!"


@host_fn
def testing_123(input, context):
    mem = context.current_plugin_memory_from_offset(input[0])
    print("Hello from Python!")
    print(context.current_plugin_memory(mem)[:])
    print(requests.get("https://example.com").text)
    return input[0]


# a Context provides a scope for plugins to be managed within. creating multiple contexts
# is expected and groups plugins based on source/tenant/lifetime etc.
with Context() as context:
    wasm = open("../wasm/code.wasm", "rb").read()
    hash = hashlib.sha256(wasm).hexdigest()
    config = {"wasm": [{"data": wasm, "hash": hash}], "memory": {"max": 5}}

    functions = [
        Function("testing_123",
                 testing_123, [ValType.I64], [ValType.I64],
                 user_data=context)
    ]
    plugin = context.plugin(config, wasi=True, functions=functions)
    # Call `count_vowels`
    j = json.loads(plugin.call("count_vowels", data))
    print("Number of vowels:", j["count"])


# Compare against Python implementation
def count_vowels(data):
    count = 0
    for c in data:
        if c in b"AaEeIiOoUu":
            count += 1
    return count


assert j["count"] == count_vowels(data)
