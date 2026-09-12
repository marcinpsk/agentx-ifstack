# CHANGELOG

<!-- version list -->

## v0.0.3 (2026-09-12)

### Bug Fixes

- Close three gaps in the guards added by this branch
  ([`f513c62`](https://github.com/marcinpsk/agentx-ifstack/commit/f513c6230b2b263fb7869de3d69b269556c8b8c7))

- Close three more gaps in this branch's own guards
  ([`0988e38`](https://github.com/marcinpsk/agentx-ifstack/commit/0988e38e529cb3306d9cc7ff7cb536ea96cf3aa7))

- Deny rustc warnings in the manifest, and reject a bare push trigger
  ([`e5674a5`](https://github.com/marcinpsk/agentx-ifstack/commit/e5674a585aee1cee9b7dfdac7ac0f849aa33fc86))

- Refuse an incomplete package set before the release exists
  ([`ef6c4a7`](https://github.com/marcinpsk/agentx-ifstack/commit/ef6c4a733a49185bfc553ffccfa96f7dc158d5e2))

- Write a Debian changelog trailer dpkg accepts
  ([`4cf483b`](https://github.com/marcinpsk/agentx-ifstack/commit/4cf483ba2254086ac3ec97e2d24f694629e892db))

### Chores

- **deps**: Bump actions/checkout from 5.1.0 to 7.0.1
  ([`4efa34b`](https://github.com/marcinpsk/agentx-ifstack/commit/4efa34b1bb11f29b698baabc4a2a3a9d4d7fa0ee))

- **deps**: Bump actions/setup-python from 6.3.0 to 7.0.0
  ([`05518c0`](https://github.com/marcinpsk/agentx-ifstack/commit/05518c082877b37985c1fb2f4cd34becc937e718))

### Continuous Integration

- Attach the packages to the release and gate the dependency tree
  ([`2009362`](https://github.com/marcinpsk/agentx-ifstack/commit/20093625d4076a319deeb1e47055ea4a6129d8bc))

- Gate the invariants clippy cannot express
  ([`88f4f75`](https://github.com/marcinpsk/agentx-ifstack/commit/88f4f7502fe68a570df74869b5e44636ae755798))

- Run the checks on pull requests only
  ([`e052ad6`](https://github.com/marcinpsk/agentx-ifstack/commit/e052ad671078ddf7a8e7fcb0c7035db9255ccc40))

### Testing

- Apply the duplicate-run policy to .yaml workflows too
  ([`00df513`](https://github.com/marcinpsk/agentx-ifstack/commit/00df5133195658bede6a41c6e1a6246561ef3fb8))


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
