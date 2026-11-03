# MediaDir MCP server

An [MCP](https://modelcontextprotocol.io) server that lets AI agents read and edit metadata in MediaDir by wrapping the server's HTTP API ([docs/API.md](../docs/API.md)). It speaks stdio and does not modify the Rust server.

Covers: libraries, items (list/search/detail/edit), custom fields and field definitions, people and credits, relations, and read-only ingest status. It intentionally has **no tools to delete items or media files**.

## Install

Python 3.10+. With `uv`:

```sh
cd mcp
uv sync            # or: uv tool install .
```

Without `uv`:

```sh
cd mcp
python3 -m venv .venv
.venv/bin/pip install -e .
```

This installs the `mediadir-mcp` command (`mcp/.venv/bin/mediadir-mcp`). The `mcp` SDK is pinned to 1.x.

## Run

```sh
MEDIADIR_URL=http://localhost:8096 .venv/bin/mediadir-mcp
```

| Env var | Default | Meaning |
|---|---|---|
| `MEDIADIR_URL` | `http://localhost:8096` | Base URL of the MediaDir server |
| `MEDIADIR_TIMEOUT` | `30` | HTTP timeout in seconds |

## Claude Code

```sh
claude mcp add mediadir -e MEDIADIR_URL=http://localhost:8096 -- /home/sortedcord/mediadir/mcp/.venv/bin/mediadir-mcp
```

Or a project `.mcp.json`:

```json
{
  "mcpServers": {
    "mediadir": {
      "command": "/home/sortedcord/mediadir/mcp/.venv/bin/mediadir-mcp",
      "env": { "MEDIADIR_URL": "http://localhost:8096" }
    }
  }
}
```

With uv instead: `"command": "uv", "args": ["run", "--directory", "/path/to/mediadir/mcp", "mediadir-mcp"]`.

## Claude Desktop

Add to `claude_desktop_config.json` (Linux: `~/.config/Claude/`, macOS: `~/Library/Application Support/Claude/`) and restart:

```json
{
  "mcpServers": {
    "mediadir": {
      "command": "/home/sortedcord/mediadir/mcp/.venv/bin/mediadir-mcp",
      "env": { "MEDIADIR_URL": "http://localhost:8096" }
    }
  }
}
```

## Tools

- Discovery: `list_libraries`, `list_items`, `search`, `get_item`
- Item metadata: `update_item`, `unlock_field`, `set_item_field`, `delete_item_field`
- Field definitions: `list_field_defs`, `create_field_def`, `update_field_def`, `delete_field_def`
- People and credits: `search_people`, `get_person`, `create_person`, `update_person`, `delete_person`, `set_person_field`, `delete_person_field`, `set_person_image`, `add_credit`, `update_credit`, `remove_credit`
- Relations: `list_relation_types`, `list_relations`, `get_relation`, `get_item_relations`, `create_relation`, `update_relation`, `delete_relation`, `add_relation_member`, `remove_relation_member`, `replace_relation_members`
- Ingest (read-only): `ingest_status`

Results are compact JSON text; API errors come back as tool errors with the server's message.

Known limits (API gaps): the API has no item artwork upload, so there is no item thumbnail tool; credits have no PATCH, so `update_credit` re-creates the credit (its id changes).

## Tests

`tests/smoke_test.py` starts a disposable server (temp dirs, port 8199, never 8096), uploads a generated ffmpeg clip, and drives this server over stdio with the MCP client SDK. Needs a built server binary and ffmpeg:

```sh
(cd ../server && cargo build --release)
.venv/bin/python tests/smoke_test.py          # optional arg: path to mediadir-server binary
```
