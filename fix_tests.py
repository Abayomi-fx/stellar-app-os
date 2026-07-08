import re

with open("contracts/tree-escrow/src/lib.rs", "r") as f:
    content = f.read()

content = content.replace(
    "client.initialize(&Address::generate(&env), &tree_token);",
    "client.initialize(&Address::generate(&env), &tree_token, &Address::generate(&env));"
)

with open("contracts/tree-escrow/src/lib.rs", "w") as f:
    f.write(content)

with open("contracts/escrow-milestone/src/lib.rs", "r") as f:
    content2 = f.read()

content2 = content2.replace(
    "client.initialize(&Address::generate(&env));",
    "client.initialize(&Address::generate(&env), &Address::generate(&env));"
)

with open("contracts/escrow-milestone/src/lib.rs", "w") as f:
    f.write(content2)

