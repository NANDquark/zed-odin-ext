# 🔨 Odin Language Support for Zed

This project provides Odin programming language support, featuring syntax highlighting and code navigation via Tree-sitter, Language Server capabilities like autocompletion and diagnostics, and full debugging support.

- Tree Sitter: [tree-sitter-odin](https://github.com/tree-sitter-grammars/tree-sitter-odin)
- Language Server: [@DanielGavin/ols](https://github.com/DanielGavin/ols)
- Debug Adapters: LLDB (Built-in)

---

## Language Server

This extension automatically updates to the latest OLS (Odin Language Server) monthly build on each startup.

### Using a Custom OLS Binary

If you want to use a specific OLS version or a locally built binary, you can override the automatic download:

```json
{
  "lsp": {
    "ols": {
      "binary": {
        "path": "/path/to/your/ols",
        "arguments": []
      }
    }
  }
}
```

### Binary Resolution Order

The extension searches for the OLS binary in the following priority order:

1. **Custom binary path** - If configured in settings (see above)
2. **System PATH** - Checks if `ols` is available in your system PATH
3. **Cached binary** - Uses previously downloaded version if available
4. **GitHub download** - Downloads latest release from [DanielGavin/ols](https://github.com/DanielGavin/ols/releases)

---

## Configuration

#### Configure via Zed Settings (Recommended)

Add OLS configuration directly in your Zed `settings.json`. This approach works project-wide and doesn't require additional files:

```jsonc
{
  "lsp": {
    "ols": {
      "initialization_options": {
        "enable_hover": true,
        "enable_snippets": true,
        "enable_procedure_snippet": true,
        "enable_completion_matching": true,
        "enable_references": true,
        "enable_document_symbols": true,
        "enable_format": true,
        "enable_document_links": true,
        "collections": [
          {
            "name": "shared",
            "path": "/path/to/shared"
          }
        ]
      }
    }
  }
}
```

#### Use `ols.json` in Workspace Root

Alternatively, create an `ols.json` file at the root of your workspace.For more configuration options, see the [OLS documentation](https://github.com/DanielGavin/ols#configuration).

---

## Snippets

You can define custom code snippets to speed up your Odin development workflow.

### Creating Snippets

1. Open the command palette (`Cmd/Ctrl+Shift+P`)
2. Run `snippets: configure snippets`
3. Create or edit `odin.json` in the snippets directory
4. Add your snippets in JSON format

Example snippet:

```json
{
  "Main procedure": {
    "prefix": "main",
    "body": [
      "package main",
      "",
      "import \"core:fmt\"",
      "",
      "main :: proc() {",
      "\t$0",
      "}"
    ],
    "description": "Creates a main package with imports"
  }
}
```

For detailed information about creating and using snippets, see [Zed's snippet documentation](https://zed.dev/docs/snippets).

---

## Debugging

This extension supports debugging Odin applications using **LLDB**.

By default the automatically generated debug tasks in Zed will load `resources/lldb/odin.py` for you, which improves the rendering of Odin types (string, slices, dynamic array, fixed capacity dynamic array, maps, and unions) within Zed.

For custom `debug.json` tasks, point LLDB at the installed extension copy of `odin.py`. Zed installs extensions under:

- macOS: `~/Library/Application Support/Zed/extensions`
- Linux: `$XDG_DATA_HOME/zed/extensions` or `~/.local/share/zed/extensions`
- Windows: `%LOCALAPPDATA%\\Zed\\extensions`

The Odin extension id is `odin`, so the script lives at `installed/odin/resources/lldb/odin.py` under that directory.

In order to use this with any custom `debug.json` tasks in Zed, add `initCommands` as shown below:

```json
  {
    "label": "Debug Odin",
    "adapter": "CodeLLDB",
    "request": "launch",
    "cwd": "$ZED_WORKTREE_ROOT",
    "program": "<your program>",
    "initCommands": [
      "command script import \"<Zed extensions dir>/installed/odin/resources/lldb/odin.py\"",
    ],
  },
```

---
