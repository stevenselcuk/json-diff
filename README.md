# json-diff

A high-performance, terminal-based JSON diff tool written in Rust. It features a side-by-side view, efficient handling of large files (using memory mapping), and an **interactive merge mode** for resolving conflicts directly in the terminal.

## Features

- **Side-by-Side Diffing**: Clear visual comparison of two files.
- **Large File Support**: Efficiently handles large JSON files (>500MB) using memory mapping and zero-copy parsing.
- **Interactive Merge**: Navigate conflicts and choose resolutions (`Ours`, `Theirs`, `Both`, or `Edit`).
- **TUI Interface**: Built with `ratatui` for a responsive terminal user interface.
- **Smart saving**: Prompts for filename and location when saving the merged output.

## Installation

### Via Installer (Recommended)

To install the latest pre-compiled binary:

```bash
curl -fsSL https://raw.githubusercontent.com/stevenselcuk/json-diff/main/install.sh | sh
```

### Via Homebrew (macOS/Linux)

No additional dependencies (like Rust) are required.

```bash
brew tap stevenselcuk/tap
brew install json-diff
```

### From Source

Requires Rust (via [rustup](https://rustup.rs/)).

````bash
git clone https://github.com/stevenselcuk/json-diff.git
cd json-diff
### Manual Distribution (Pre-compiled Binary)

If you downloaded the binary directly:
1.  Make it executable: `chmod +x json-diff`
2.  Move it to your path: `mv json-diff /usr/local/bin/` (or anywhere in `$PATH`)

## Building for Distribution

To create a release build manually:

1.  Build the release binary:
    ```bash
    cargo build --release
    ```
2.  The binary will be at `target/release/json-diff`.
3.  You can zip this file and distribute it. Users just need to download and run it.

## Usage

Run the tool by providing two file paths:

```bash
json-diff <file1> <file2>
````

**Example:**

```bash
json-diff source_v1.json source_v2.json
```

## User Guide & Key Bindings

### Navigation

| Key         | Action                                              |
| :---------- | :-------------------------------------------------- |
| `▼` / `j`   | Scroll Down (1 line)                                |
| `▲` / `k`   | Scroll Up (1 line)                                  |
| `PgDn`      | Scroll Down (1 page)                                |
| `PgUp`      | Scroll Up (1 page)                                  |
| `Home`      | Jump to Top                                         |
| `End`       | Jump to Bottom                                      |
| `n`         | **Next Conflict** (Jump to next difference)         |
| `p`         | **Previous Conflict** (Jump to previous difference) |
| `q` / `Esc` | Quit                                                |

### Conflict Resolution (Interactive Merge)

When a difference/conflict is selected (highlighted line numbers):

| Key         | Resolution     | Result                                          |
| :---------- | :------------- | :---------------------------------------------- |
| `1` / `←`   | **Pick Left**  | Keep content from File 1 (Base/Ours).           |
| `2` / `→`   | **Pick Right** | Keep content from File 2 (Remote/Theirs).       |
| `3`         | **Pick Both**  | Keep File 1 content followed by File 2 content. |
| `Backspace` | **Reset**      | Mark as Unresolved (Default).                   |

### Saving

| Key | Action                 |
| :-- | :--------------------- |
| `s` | **Save Merged Output** |

When you press `s`, a popup will appear asking for the filename.

- **Default**: `merged_output.json` (in the current directory).
- **Action**: Type a new name or path and press `Enter` to save. Press `Esc` to cancel.

## How to Release

To simplify the release process, use the provided script:

1.  Run `./package.sh`
    - This builds the release binary.
    - It creates a tarball (e.g., `json-diff-arm64-apple-darwin.tar.gz`) for your current OS/Arch.
2.  Go to GitHub Releases.
3.  Create a new release (e.g., `v0.1.0`).
4.  Upload the `.tar.gz` file created by the script.

Users can then install it automatically using the `curl` command above.
