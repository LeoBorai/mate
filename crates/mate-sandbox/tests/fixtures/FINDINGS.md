# Image contract spike

Captured on the host with Claude Code 2.1.325, **not** from the sandbox image: this
environment has no Docker, so the image was never built or run.

## Captured (host)

- `claude --help` lists `-p, --print`, `--output-format <format>` (only with `--print`),
  `-r, --resume [value]`, and `--session-id <uuid>` (`claude_help_excerpt.txt`).
- `claude_p_success.json`: stdout of `claude -p "Reply with exactly: ok" --output-format json`,
  exit 0, empty stderr. Fields used by `parse_headless`: `type` ("result"), `subtype`,
  `is_error` (false), `result` ("ok"), `session_id` (UUID).
- `claude_p_not_logged_in.json`: same command with an empty `CLAUDE_CONFIG_DIR`. Exit 1,
  empty stderr, stdout is JSON with `is_error: true`,
  `result: "Not logged in · Please run /login"`, and still a `session_id`.

## Still to capture in the built image (needs Docker)

`docker build -t mate-sandbox:spike crates/mate-sandbox/image`, then in a throwaway container:

- `Config.User` (`docker image inspect -f '{{.Config.User}}'`) and `echo $HOME`
- `ls -ld ~/.claude` (exists, owned by the image user)
- `tmux -V`, `tmux show -g status` (expect `status off`)
- `claude --version` and `claude --help | grep -E 'session-id|resume|output-format'`
  in the image's own Claude Code version
