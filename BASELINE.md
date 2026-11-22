# Test baseline (macOS arm64, rustc 1.100.0)

`cd jarvis && cargo test --locked -p jarvis`

- Before changes (tag v0.10.16): 107 passed, 0 failed.
- After Task 1 (adds `updates::tests::custom_build_is_locked`): 108 passed, 0 failed.
- No tests fail on macOS. "Green" means: not worse than this.
- Note: the panel UI must exist first (`npm ci && npm run build:panel`), otherwise the core build script panics.

## Soloist seek (checked on the speaker, Task 3)

- `soloist ctl --help` lists `seek <ms>` (position in milliseconds). `ctl seek --help` just prints the same usage.
- Syntax used: `LD_LIBRARY_PATH=/data/opt/soloist/lib64 /data/opt/soloist/bin/soloist ctl -w 127.0.0.1:9090 seek <ms>`.
- `scripts/control.sh` does not accept `seek` (exit 2), so jarvis calls the binary directly (`spotify::seek`).
- After Task 3 the test count is 123 passed, 0 failed.
