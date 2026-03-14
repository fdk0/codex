# Slash commands

For an overview of Codex CLI slash commands, see [this documentation](https://developers.openai.com/codex/cli/slash-commands).

## Session thread navigation

### `/follow-thread [on|off|status]`

Controls session-local auto-follow for live agent threads in the current Codex run.

- `on`: automatically switch the visible thread to the latest live agent thread in the current session
- `off`: disable auto-follow
- `status`: show whether auto-follow is currently enabled

Notes:

- This only applies to the current running Codex session.
- It does not scan saved session files on disk.
- It does not switch across unrelated historical sessions.
