# Online snapshot test cleanup

GitHub issue [#419](https://github.com/markdlabrecque/wayfinder/issues/419) identified an
environment-dependent hang in `tests/online_snapshot.rs`.

`Server::drop` attempted to launch an external `kill` executable and discarded the launch
result before calling the blocking `Child::wait`. In minimal containers without that executable,
the healthy server received no signal, so the wait—and therefore the test suite—never ended.
The successful environment differed only because its external command delivered SIGTERM; the
server itself was not deadlocked.

The fixture now uses `std::process::Child::kill`, which has no executable dependency. It waits
to reap the process only after termination was successfully requested, and reports inspection,
termination, or wait failures to stderr rather than silently converting a termination failure
into an unbounded wait. SIGKILL is appropriate for these test-only fixtures; graceful SIGTERM
behavior remains covered separately by `tests/ops_shutdown.rs`.

Validation included `cargo test --test online_snapshot` in `rust:1-slim-bookworm`, where no
external `kill` executable is installed, plus the repository formatting, lint, and test gates.
No focused executable unit regression was added because the existing integration test exercises
the complete fixture cleanup path and the missing-executable condition is environmental.
