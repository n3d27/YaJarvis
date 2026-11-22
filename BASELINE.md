# Test baseline (macOS arm64, rustc 1.100.0)

`cd jarvis && cargo test --locked -p jarvis`

- Before changes (tag v0.10.16): 107 passed, 0 failed.
- After Task 1 (adds `updates::tests::custom_build_is_locked`): 108 passed, 0 failed.
- No tests fail on macOS. "Green" means: not worse than this.
- Note: the panel UI must exist first (`npm ci && npm run build:panel`), otherwise the core build script panics.
