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
| `--root <PATH>` | Extra root directory to scan, repeatable. |
| `--all` | Show everything, including tracked items and known state and cache items. |
| `--json` | Emit JSON instead of the human report. |
| `--no-color` | Disable colored output. |
| `--fail-on <LIST>` | Comma separated conditions that give a non-zero exit: `secrets`, `drift`, `untracked`. Default `secrets,drift`. |

By default dredge walks the top level dot entries of your home directory plus `~/.config`, `~/Library/Application Support` and `~/Library/Preferences`.

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

Place a `.dredgeignore` file at the root of the dotfiles repository to list paths that are intentionally untracked. They are left out of the untracked list and the exit code, and counted in the summary line.

The dialect is deliberately small:

- One glob per line. Blank lines and lines starting with `#` are skipped. Surrounding whitespace is trimmed.
- Patterns are matched against the path relative to the home directory (for example `.config/nvim/**` or `.zsh_sessions`), not relative to the dotfiles repository.
- A pattern matches a path if it matches the path itself or any of its parent directories. `dir/**` also matches `dir` itself.
- Glob matching uses the `globset` defaults, so `*` can cross `/`.
- No negation: a leading `!` is not special. No handling of a leading `/` (anchoring) or trailing `/` (directories only): patterns containing them will simply not match as in gitignore.
- Lines that are not valid globs are silently skipped.
- The file must be a regular file of at most 64 KiB; anything else is treated as an empty ignore list.
- Ignoring never hides secrets: an ignored item that contains a secret is still reported (marked as ignored) and still affects the exit code under `secrets`.

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
