# dredge

dredge is a read-only command line tool for macOS that looks at your home directory and tells you what your dotfiles repository does not know about. It reports configuration that is not tracked, tracked files that have drifted from the live copy, and secrets (private keys, API tokens) sitting in live config files or already committed to your dotfiles repository. It works with stow layouts, chezmoi source directories, or no dotfiles manager at all. The package name `dredge` is a placeholder.

## Install

There is no release yet. Build from source with a stable Rust toolchain:

```sh
git clone <this repository>
cd dredge
cargo build --release
./target/release/dredge scan --help
```

## Usage

```sh
dredge scan --dotfiles ~/dotfiles
```

| Flag | Meaning |
| --- | --- |
| `--dotfiles <PATH>` | Path to the dotfiles repository. If omitted, only chezmoi's default source directory (`~/.local/share/chezmoi`) is auto-detected. |
| `--layout <auto\|stow\|chezmoi>` | Repository layout. `auto` (default) picks chezmoi if the directory contains `.chezmoiroot`, `.chezmoiignore`, or names starting with `dot_`, otherwise stow. |
| `--root <PATH>` | Extra root directory to scan, repeatable. See "Limits" for roots that overlap. |
| `--all` | Show everything, including tracked items and known state and cache items. |
| `--json` | Emit JSON instead of the human report. |
| `--no-color` | Disable colored output. |
| `--no-progress` | Never show the progress line on stderr. |
| `--fail-on <LIST>` | Comma separated conditions that give a non-zero exit: `secrets`, `drift`, `untracked`. Default `secrets,drift`. |

By default dredge walks the top level dot entries of your home directory plus `~/.config`, `~/Library/Application Support` and `~/Library/Preferences`.

## Limits

- Each depth-1 entry under a root is one unit (for example `~/.config/nvim`). Inside a unit the walk goes at most 6 levels deep (a file directly inside the unit directory is level 1); anything deeper is not listed.
- Only the home directory root is limited to dot entries. Every other root, including each `--root`, lists all of its depth-1 entries.
- The committed-secrets scan of the dotfiles working tree descends at most 16 levels.
- The human report lists at most 20 unknown units, with the total count in the header. `--all` lists every one.
- The dotfiles directory is never reported as home content, even when it lives inside a scanned root. Its contents are only compared against the live files and scanned for committed secrets.
- Files under `~/Library/Preferences` are listed by name only and are never scanned for secrets.
- A `--root` that is already covered by another root is not walked or listed twice. That means it lies inside a part of that root that is walked, or it is the same path as another root, including when that path is a symlink (for example `--root X` given twice, or `--root ~/.config` when `~/.config` is a symlink). dredge prints one `dredge: note:` line on stderr naming both and the report is the same as without the extra `--root`. A `--root` inside the dotfiles directory is skipped the same way. A `--root` that contains other roots is walked normally, leaving out the subtrees that are themselves roots.
- Directories that sit at the depth cap are not entered. When there are any, the IGNORED line of the human report is followed by `N directories deeper than 6 levels were not scanned`, and the JSON has the count in `depth_truncated_dirs` (always present, 0 when none).
- Config and unknown files over 1 MiB are not scanned for secrets. When there are any, the human report adds `N files over 1 MiB were not scanned for secrets` under IGNORED, and the JSON has the count in `skipped_too_large` (always present, 0 when none). Binary files are skipped without being counted. Neither count changes the exit code.
- Cache and state directories are not entered. A directory is skipped when an app rule says its contents are `cache` or `state`, or, when no app rule covers it, when its name is a recognised cache or state name, compared ignoring ASCII case (so `Cache`, `cache` and `CACHE` all match): `Caches`, `cache`, `.cache`, `node_modules`, `logs`, `tmp`, `GPUCache`, `Crashpad`, the Chromium and Electron storage directories `Code Cache`, `DawnCache`, `DawnGraphiteCache`, `DawnWebGPUCache`, `GrShaderCache`, `ShaderCache`, `Service Worker`, `IndexedDB`, `Local Storage`, `Session Storage`, `blob_storage`, `File System`, `WebStorage`, `Shared Dictionary`, `component_crx_cache` and `Crash Reports`, and the editor directories `CachedData`, `CachedExtensions` and `CachedExtensionVSIXs`. An app rule of kind `config` that covers such a directory keeps it walked.
- Files inside a skipped directory are never listed, opened, read or scanned for secrets, even when the directory sits inside an otherwise unknown app folder, so a secret stored there is not reported. The rest of the unit is still read and scanned, and so are unknown files everywhere else. A unit whose own directory is skipped (for example `~/.cache`) is still listed, as cache or state, under CACHE/STATE with `--all`. A `--root` that is or lies inside a skipped directory is walked as its own root.
- When any directory was skipped, the IGNORED line of the human report is followed by `N cache/state directories were not entered`. Each skipped directory counts once and nothing inside it is visited, so nested ones are not counted. The JSON has the count in `pruned_dirs` (always present, 0 when none). The `cache/state items` count on the IGNORED line (`cache_state_items` in the JSON) counts only the files that were listed in cache and state units, so it never includes files inside skipped directories. Skipped directories are not counted as depth-capped directories.
- By default units are scanned on one worker thread per CPU, at most 8. The hidden `--jobs <N>` flag (N at least 1) sets the number of workers, for example `--jobs 1`; the report is the same for any value. The walk and the committed-secrets scan each run on one thread.
- dredge shows a one line progress count on stderr, only when stderr is a terminal: `dredge: walking... N entries` while it lists directories, then `dredge: scanned N files...` while it reads them. It is cleared before the report is printed, never contains a path, and `--no-progress` turns it off. When stderr is a pipe or a file nothing extra is written.

Sample output, from a run against a synthetic fixture home (a few made up files in a temporary directory, not a real home):

```
COMMITTED SECRETS
  (none)

UNTRACKED SECRETS
  ngrok          ~/.config/ngrok/ngrok.yml                    SECRET  rule: ngrok-authtoken, line 2

UNTRACKED CONFIG
  ngrok          ~/.config/ngrok                              SECRET
  neovim         ~/.config/nvim
  starship       ~/.config/starship.toml

TRACKED, DRIFTED
  zsh            ~/.zshrc                                     differs from dots/zsh/.zshrc

UNKNOWN (0)
  (none)

COULD NOT READ
  (none)

IGNORED  1 cache/state items, 0 ignored by .dredgeignore
```

Findings name the rule, file and line. Secret values never appear in any output.

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | None of the `--fail-on` conditions was hit. |
| 1 | At least one selected condition was hit (secrets, drift, or untracked items). |
| 2 | The tool itself failed (for example, a missing dotfiles directory). |

Untracked configuration alone exits 0 unless you pass `--fail-on untracked`.

## .dredgeignore

Place a `.dredgeignore` file at the root of the dotfiles repository to list paths that are intentionally untracked. Ignored paths are excluded from the untracked list only (and so from `--fail-on untracked`); drift and secrets are always reported. Each ignored item is counted in the summary line.

The dialect is deliberately small:

- One glob per line. Blank lines and lines starting with `#` are skipped. Surrounding whitespace is trimmed.
- Patterns are matched against the path relative to the home directory (for example `.config/nvim/**` or `.zsh_sessions`), not relative to the dotfiles repository.
- A pattern matches a path if it matches the path itself or any of its parent directories. `dir/**` also matches `dir` itself.
- Glob matching uses the `globset` defaults, so `*` can cross `/`.
- No negation: a leading `!` is not special. No handling of a leading `/` (anchoring) or trailing `/` (directories only): patterns containing them will simply not match as in gitignore.
- Lines that are not valid globs are silently skipped.
- The file must be a regular file of at most 64 KiB; anything else is treated as an empty ignore list.
- Ignoring never hides secrets or drift: an ignored item that contains a secret is still reported (marked as ignored) and still affects the exit code under `secrets`, and an ignored tracked file that has drifted is still listed under TRACKED, DRIFTED and still affects the exit code under `drift`.

## Safety guarantees

- Read-only. dredge never writes, moves or deletes anything.
- No network access.
- No secret values in any output: findings carry only the rule id, path and line number.
- Symlinks are listed but never followed, including symlinks pointing out of the scanned roots.
- Bounded reads: files over 1 MiB and binary files (containing NUL bytes) are not scanned, and the walk depth is capped.
- All secret patterns use the linear-time `regex` crate engine (no lookaround, no backtracking).

## What v1 does not do

- No `adopt` command: it does not write stow or chezmoi packages for you.
- No interactive TUI.
- macOS only.
- No git history scan: "committed secrets" means the current working tree of the dotfiles repository, not past commits.
- No entropy based secret detection. The generic rule is shipped disabled.

## Stow and chezmoi support

Stow: packages are the top level directories of the dotfiles repository that do not start with a dot, and their contents are compared with the live files.

chezmoi: source names are mapped to target paths and contents are compared. Source entries whose names start with a dot (such as `.git`) are ignored, and `.chezmoiroot` is honored when it names a plain subdirectory. The attributes `dot_`, `private_`, `executable_` and `readonly_` are fully supported. Files with the `.tmpl` suffix are reported as tracked without drift comparison. Entries using `symlink_`, `exact_` or `encrypted_` are reported as `tracked (unverified)`, and so is everything inside a directory marked `exact_`, `symlink_`, `encrypted_`, `create_`, `modify_` or `run_`. Files using `run_`, `modify_` or `create_` are skipped entirely, since they are scripts or templates with no sensible target.

## Contributing an app rule

App rules live in `rules/apps.toml`. Adding one is three lines of TOML plus a description:

```toml
[[rule]]
app = "myapp"
kind = "config"            # config, state or cache
globs = [".config/myapp/**"]
description = "myapp configuration"
```

Globs are relative to the home directory, use `/` separators, and `*` does not cross `/` (use `dir/**` for recursion). The first matching rule wins, so put narrower rules before broader ones. Only add paths you are sure about: an unclassified path is reported as unknown, which is better than a wrong classification. `cargo test` checks that every rule is complete, that no glob is duplicated, and that no glob is shadowed by an earlier rule.

Secret rules live in `rules/secrets.toml`. A new enabled rule needs a precise pattern and a detection case (a fake sample that fires only that rule, and a near miss that does not) in `tests/rules_schema.rs`. Build fake values from fragments so the repository holds no complete token-shaped string.

## License

MIT OR Apache-2.0. See `LICENSE-MIT`, `LICENSE-APACHE` and `NOTICE-THIRD-PARTY.md`.
