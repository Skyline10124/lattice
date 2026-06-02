# Legacy Dogfood Scripts

These scripts preserve the original Phase 22 and manual Phase 27 live harnesses.
They remain useful for historical reproduction and behavior comparison, but the
Phase 22 prompts/assertions use legacy pre-v1.3.1 tool names.

Current benchmark runs should use:

```sh
benchmarks/lancet-dogfood/run.sh
```

Legacy scripts in this directory:

- `p22-live-smoke.sh`
- `p22-real-scenarios.sh`
- `p22-self-host.sh`
- `p22-stress.sh`
- `p27-real-dogfood-manual.sh`
