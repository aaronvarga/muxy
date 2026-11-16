# Claude stream fixtures

Recorded in Milestone 0 from `claude` 2.1.319 on macOS, then redacted (paths, account, plugin lists, signatures).

Each line is one record: `{"dir": "in"|"out"|"err"|"meta", "t": seconds, "msg": <JSON line>}`.
`in` is what the host wrote to the child's stdin, `out` is the child's stdout, `err` is stderr (`raw`),
and `meta` holds the argv and the exit code. Streamed tool input (`input_json_delta`) is redacted as a
whole: the full JSON sits in the first chunk and later chunks are empty.

Flags for every run: `-p --input-format stream-json --output-format stream-json --verbose
--include-partial-messages --permission-prompt-tool stdio --model haiku`, unless `meta.argv` says otherwise.
