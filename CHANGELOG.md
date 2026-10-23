# CHANGELOG

<!-- version list -->

## v0.2.5 (2026-10-23)

### Bug Fixes

- **deps**: Bump the uv group across 1 directory with 2 updates
  ([#42](https://github.com/marcinpsk/agentx-ifstack/pull/42),
  [`828aebf`](https://github.com/marcinpsk/agentx-ifstack/commit/828aebf064eb46082f566094d4e5e16aba4a5f4b))

### Chores

- **deps**: Bump actions/download-artifact from 8.0.1 to 8.0.2
  ([#34](https://github.com/marcinpsk/agentx-ifstack/pull/34),
  [`126e812`](https://github.com/marcinpsk/agentx-ifstack/commit/126e812a5aa9e1f568b91451f4eb0882468b36b2))

- **deps**: Bump actions/upload-artifact from 7.0.1 to 7.0.2
  ([#31](https://github.com/marcinpsk/agentx-ifstack/pull/31),
  [`3f10077`](https://github.com/marcinpsk/agentx-ifstack/commit/3f10077b63da0edc7792721c958efe27fc7ee521))

- **deps**: Bump astral-sh/setup-uv from 10.2.0 to 10.3.0
  ([#30](https://github.com/marcinpsk/agentx-ifstack/pull/30),
  [`044a886`](https://github.com/marcinpsk/agentx-ifstack/commit/044a886126f1a713419c78560fc79e60eae132a3))

- **deps**: Bump libc from 0.2.189 to 0.2.190
  ([#35](https://github.com/marcinpsk/agentx-ifstack/pull/35),
  [`c7fa439`](https://github.com/marcinpsk/agentx-ifstack/commit/c7fa4397a65f6fae0415852d0a497460e5fd467c))

- **deps**: Bump rust-toolchain from 1.99.0 to 1.99.1
  ([#32](https://github.com/marcinpsk/agentx-ifstack/pull/32),
  [`6432e55`](https://github.com/marcinpsk/agentx-ifstack/commit/6432e5581ff19c76188567f42b1ab7917aa37cc5))

- **deps**: Bump serde_json from 1.0.151 to 1.0.154
  ([#41](https://github.com/marcinpsk/agentx-ifstack/pull/41),
  [`4830e21`](https://github.com/marcinpsk/agentx-ifstack/commit/4830e218f384d40db9f3236ac57c640d85226a54))

- **deps**: Bump toml from 1.1.6+spec-1.1.0 to 1.1.8+spec-1.1.0
  ([#33](https://github.com/marcinpsk/agentx-ifstack/pull/33),
  [`d4f4861`](https://github.com/marcinpsk/agentx-ifstack/commit/d4f486102fe02161629e4d2c5ac7e97d75d66899))

- **deps**: Bump zizmorcore/zizmor-action from 0.6.4 to 0.7.1
  ([#40](https://github.com/marcinpsk/agentx-ifstack/pull/40),
  [`3b5c7b8`](https://github.com/marcinpsk/agentx-ifstack/commit/3b5c7b81ea6201c817b83b05a146065b52148ad2))

### Testing

- Prevent executable-copy races in namespace tests
  ([#39](https://github.com/marcinpsk/agentx-ifstack/pull/39),
  [`d782833`](https://github.com/marcinpsk/agentx-ifstack/commit/d78283340968e5aacb51bcb771c68388add3df38))


## v0.2.4 (2026-10-17)

### Bug Fixes

- Systemd readiness reporting ([#37](https://github.com/marcinpsk/agentx-ifstack/pull/37),
  [`f8483fc`](https://github.com/marcinpsk/agentx-ifstack/commit/f8483fca06ba75d70086480d87c95038eea12dae))

### Chores

- **deps**: Bump astral-sh/setup-uv from 10.1.0 to 10.2.0
  ([#28](https://github.com/marcinpsk/agentx-ifstack/pull/28),
  [`291eb36`](https://github.com/marcinpsk/agentx-ifstack/commit/291eb365e602db8f0f71e8240b23a25d3cd7faa0))

- **deps**: Bump rust-toolchain from 1.98.1 to 1.99.0
  ([#27](https://github.com/marcinpsk/agentx-ifstack/pull/27),
  [`9a4ddda`](https://github.com/marcinpsk/agentx-ifstack/commit/9a4ddda8bff4598635caf2e453ea525064551b7d))

- **deps**: Bump urllib3 from 2.7.0 to 2.8.0 in the uv group across 1 directory
  ([#29](https://github.com/marcinpsk/agentx-ifstack/pull/29),
  [`220f57c`](https://github.com/marcinpsk/agentx-ifstack/commit/220f57cca59f21363212e0c8f014da681d1a857b))


## v0.2.3 (2026-09-22)

### Bug Fixes

- Topology ([#25](https://github.com/marcinpsk/agentx-ifstack/pull/25),
  [`17c9efa`](https://github.com/marcinpsk/agentx-ifstack/commit/17c9efab884f8a5e99e06134f2207e7ad31673e7))


## v0.2.2 (2026-09-17)

### Bug Fixes

- Harden AgentX validation and package checks
  ([#24](https://github.com/marcinpsk/agentx-ifstack/pull/24),
  [`0b3971e`](https://github.com/marcinpsk/agentx-ifstack/commit/0b3971e31917e249dc284a5285db15a93a95d5ec))


## v0.2.1 (2026-09-14)

### Bug Fixes

- Accept an acknowledgement carrying a VarBindList
  ([#13](https://github.com/marcinpsk/agentx-ifstack/pull/13),
  [`f931fcf`](https://github.com/marcinpsk/agentx-ifstack/commit/f931fcfd21e23b5fb6626861a110736bf551a0b0))


## v0.2.0 (2026-09-14)

### Documentation

- The netlink monitoring design and implementation handoff
  ([#9](https://github.com/marcinpsk/agentx-ifstack/pull/9),
  [`f819fb9`](https://github.com/marcinpsk/agentx-ifstack/commit/f819fb9f5ef3e75c8f3ff768cd6cad010abd7dae))

### Features

- Netlink ([#12](https://github.com/marcinpsk/agentx-ifstack/pull/12),
  [`522e44d`](https://github.com/marcinpsk/agentx-ifstack/commit/522e44d76ccf7bc973d4bf75f41c14369ec83abe))


## v0.1.0 (2026-09-12)

### Bug Fixes

- Fetch body in the frontier child query, and guard prose against command
  ([`09139b9`](https://github.com/marcinpsk/agentx-ifstack/commit/09139b94fbb0b569c04d49abea21210a60fc7b8b))

- Let a merge commit subject bump the version
  ([`3e0dd61`](https://github.com/marcinpsk/agentx-ifstack/commit/3e0dd618a61d5ee4cd84a97f3dcc2064c517f270))

- Parse documented commands once, and page the external-PR query correctly
  ([`6819217`](https://github.com/marcinpsk/agentx-ifstack/commit/6819217724e63eeafc9b281fb0f08fa5b901a0fb))

### Continuous Integration

- Audit the whole repo with zizmor, and give Dependabot a cooldown
  ([`6a503e5`](https://github.com/marcinpsk/agentx-ifstack/commit/6a503e556c151ad81fd38a8277bb7ae88127e1d1))

- Move opengrep to a pre-commit hook and audit the workflows with zizmor
  ([`c35b8bd`](https://github.com/marcinpsk/agentx-ifstack/commit/c35b8bdd42aff734af835d2d9202b8af34118ad7))

### Documentation

- Correct the gh commands in the issue tracker skill
  ([`8db02c3`](https://github.com/marcinpsk/agentx-ifstack/commit/8db02c31e157c92bfafcf2de9682a530605a44f4))

- Record the issue tracker, triage labels and domain doc layout
  ([`c8b3a74`](https://github.com/marcinpsk/agentx-ifstack/commit/c8b3a7428082778bfae8a37135d4bb64eeb8fe5e))

### Features

- Record the issue tracker, triage labels and domain doc layout
  ([#5](https://github.com/marcinpsk/agentx-ifstack/pull/5),
  [`cab626f`](https://github.com/marcinpsk/agentx-ifstack/commit/cab626fd471ce9b7fd79e10af2dcd09c8f4c2845))

### Testing

- Check every documented gh --json field against the real CLI
  ([`2e33899`](https://github.com/marcinpsk/agentx-ifstack/commit/2e338996adc3d8b2365bd88874da1f8d2442f807))


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
