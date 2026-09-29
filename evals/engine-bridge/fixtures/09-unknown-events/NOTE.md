# 09-unknown-events: reviewer notes

Unknown-event warning visibility is currently only printed via `eprintln` to stderr and cannot be asserted in-process. `expected.json` therefore covers only that the run tolerates unknown events and completes normally, not that the warning is visible.
