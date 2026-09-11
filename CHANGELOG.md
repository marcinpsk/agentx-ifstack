# CHANGELOG

<!-- version list -->

## v0.0.2 (2026-09-11)

### Bug Fixes

- Bound ip cleanup, make version sync crash-safe, drop the release credential
  ([`b65465e`](https://github.com/marcinpsk/agentx-ifstack/commit/b65465e46bef270df4e99519bc240e5c67d4d044))

- Bound the ip reader threads and the process group kill
  ([`d5e3198`](https://github.com/marcinpsk/agentx-ifstack/commit/d5e31982838fbddbd7936f665351539397b831ce))

- Bound the ip subprocess, isolate package temp files, pin actions
  ([`12ed47c`](https://github.com/marcinpsk/agentx-ifstack/commit/12ed47c88524d172c98fcf0f38393e3fa77112e8))

- Keep one odd interface from failing the whole table
  ([`297353e`](https://github.com/marcinpsk/agentx-ifstack/commit/297353e11264e5de10da8cc007a9fd4374c46fff))

- Read the vxlan underlay from linkinfo.info_data
  ([`8d9bf1f`](https://github.com/marcinpsk/agentx-ifstack/commit/8d9bf1f819acff9f5deb6e4da836d255a8150ddf))

- Sync the parent directory after replacing a version file
  ([`08a7475`](https://github.com/marcinpsk/agentx-ifstack/commit/08a7475de73beaf9b3089a645925932c9c9d521f))

- **ci**: Sync Cargo.lock without a populated cargo registry cache
  ([`58d2066`](https://github.com/marcinpsk/agentx-ifstack/commit/58d206643da2317959fb01acee93bf21d862fc36))

### Chores

- Stop tracking compiled python bytecode
  ([`bbdbf12`](https://github.com/marcinpsk/agentx-ifstack/commit/bbdbf1299a94f0823a8924deab7ef1c80f592a6e))

### Continuous Integration

- Analyse squashed commit bodies when computing the version
  ([`9fa35f2`](https://github.com/marcinpsk/agentx-ifstack/commit/9fa35f2d80c2251e02c89927f83e5d85c0f53bc2))

- Cut releases with semantic-release from the commit history
  ([`59e2d1e`](https://github.com/marcinpsk/agentx-ifstack/commit/59e2d1e83586c9afeea7ccfa66663f9db9a7d6a8))

### Testing

- Allow the descendant kill the same time the leaked-descendant test allows
  ([`0c71785`](https://github.com/marcinpsk/agentx-ifstack/commit/0c7178557e060c599e101a2109beb3d9a99e4f17))

- Prove the parent sync targets the directory, and raise ip test timeouts
  ([`3d5ba5b`](https://github.com/marcinpsk/agentx-ifstack/commit/3d5ba5b3fb4084f157419871e253c840f42e8f7c))


## v0.0.1 (2026-09-11)

- Initial Release
