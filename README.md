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

A repository that holds several stow directories (for example one shared directory plus one per machine profile) is compared by passing each directory that applies to this machine:

```sh
dredge scan --dotfiles ~/genesis/dotfiles-common --dotfiles ~/genesis/dotfiles-personal
```

| Flag | Meaning |
| --- | --- |
| `--dotfiles <PATH>` | Path to a dotfiles directory, repeatable. Every path must be an existing directory, otherwise dredge prints one error line naming it and exits 2. The same directory given twice is used once. If omitted, only chezmoi's default source directory (`~/.local/share/chezmoi`) is auto-detected. |
| `--layout <auto\|stow\|chezmoi>` | Repository layout, applied to every `--dotfiles` directory. `auto` (default) decides for each directory on its own: chezmoi if it contains `.chezmoiroot`, `.chezmoiignore`, or names starting with `dot_`, otherwise stow. |
| `--root <PATH>` | Extra root directory to scan, repeatable. See "Limits" for roots that overlap. |
| `--all` | Show everything, including tracked items and known state and cache items. |
| `--json` | Emit JSON instead of the human report. |
| `--no-color` | Disable colored output. |
| `--no-progress` | Never show the progress line on stderr. |
| `--fail-on <LIST>` | Comma separated conditions that give a non-zero exit: `secrets`, `drift`, `untracked`. Default `secrets,drift`. |

By default dredge walks the top level dot entries of your home directory plus `~/.config`, `~/Library/Application Support` and `~/Library/Preferences`.

With several `--dotfiles` directories, each one is indexed on its own. A home symlink that resolves into any of them is tracked. A regular home file is compared with the first directory, in the order given, that defines it: when two directories define the same file, the earlier `--dotfiles` wins and drift is measured against its copy. The "in repo, missing on this machine" list is the union across all directories.

## Limits

- Each depth-1 entry under a root is one unit (for example `~/.config/nvim`). Inside a unit the walk goes at most 6 levels deep (a file directly inside the unit directory is level 1); anything deeper is not listed.
- Only the home directory root is limited to dot entries. Every other root, including each `--root`, lists all of its depth-1 entries.
- The committed-secrets scan descends at most 16 levels into each dotfiles directory. A file reachable from two listed directories (one inside the other) is scanned and reported once.
- The committed-secrets scan skips files that git would ignore. It honors the `.gitignore` files inside each dotfiles directory and, when that directory lies inside a git work tree, the `.gitignore` files of the directories from the work tree's root down to it, so a root pattern such as `**/local.env` also applies when you pass a stow directory inside the repository. The work tree root is the nearest of the directory itself and its parents (at most 64 levels up) that holds a `.git` directory or a `.git` file; a symlinked `.git` does not count. No ignore file above that root is ever read, `.git/info/exclude` and your global git ignore file are not consulted, and when there is no work tree only the `.gitignore` files inside the directory apply. A `.gitignore` above the directory is read only when it is a regular file of at most 1 MiB. One difference from git: a `!` pattern in a `.gitignore` inside the directory cannot re-include a file that a `.gitignore` above the directory ignores.
- The human report lists at most 20 unknown units, with the total count in the header. `--all` lists every one.
- The human report lists at most 20 entries under IN REPO, MISSING ON THIS MACHINE. When there are more, the header shows the total, for example `IN REPO, MISSING ON THIS MACHINE (25)  showing 20, run with --all to list`, and `--all` lists every one. The JSON `missing_on_machine` array always holds the full list.
- No dotfiles directory is ever reported as home content, even when it lives inside a scanned root. Its contents are only compared against the live files and scanned for committed secrets. In the JSON, `dotfiles` is an array of every directory used (empty when there is none).
- Files under `~/Library/Preferences` are listed by name only and are never scanned for secrets.
- A `--root` that is already covered by another root is not walked or listed twice. That means it lies inside a part of that root that is walked, or it is the same path as another root, including when that path is a symlink (for example `--root X` given twice, or `--root ~/.config` when `~/.config` is a symlink). dredge prints one `dredge: note:` line on stderr naming both and the report is the same as without the extra `--root`. A `--root` inside any dotfiles directory is skipped the same way. A `--root` that contains other roots is walked normally, leaving out the subtrees that are themselves roots.
- Directories that sit at the depth cap are not entered. When there are any, the IGNORED line of the human report is followed by `N directories deeper than 6 levels were not scanned`, and the JSON has the count in `depth_truncated_dirs` (always present, 0 when none).
- Config and unknown files over 1 MiB are not scanned for secrets. When there are any, the human report adds `N files over 1 MiB were not scanned for secrets` under IGNORED, and the JSON has the count in `skipped_too_large` (always present, 0 when none). Binary files are skipped without being counted. Neither count changes the exit code.
- Cache and state directories are not entered. A directory is skipped when an app rule says its contents are `cache` or `state`, or, when no app rule covers it, when its name is a recognised cache or state name, compared ignoring ASCII case (so `Cache`, `cache` and `CACHE` all match): `Caches`, `cache`, `.cache`, `node_modules`, `logs`, `tmp`, `GPUCache`, `Crashpad`, the Chromium and Electron storage directories `Code Cache`, `DawnCache`, `DawnGraphiteCache`, `DawnWebGPUCache`, `GrShaderCache`, `ShaderCache`, `Service Worker`, `IndexedDB`, `Local Storage`, `Session Storage`, `blob_storage`, `File System`, `WebStorage`, `Shared Dictionary`, `component_crx_cache` and `Crash Reports`, and the editor directories `CachedData`, `CachedExtensions` and `CachedExtensionVSIXs`. Only the unit's own directory and the directories below it count: a cache-like name above the unit, for example in the path of a `--root`, does not. An app rule of kind `config` that covers the whole directory (`dir/**`) keeps it walked.
- Files inside a skipped directory are never listed, opened, read or scanned for secrets, even when the directory sits inside an otherwise unknown app folder, so a secret stored there is not reported. In particular, Electron and Chromium storage folders (`Local Storage`, `IndexedDB`, `Session Storage`, `Service Worker` and the like) can hold live app tokens: dredge classifies them as cache or state, never enters them, and so does not scan them for secrets. Earlier versions of dredge did scan them, as unknown files. The same holds for Visual Studio Code's local file history (`~/Library/Application Support/Code/User/History`), its workspace and global extension storage (`User/workspaceStorage`, `User/globalStorage`) and its installed extensions (`~/.vscode/extensions`): they are state, never entered, and a token saved there is not reported. `User/settings.json` next to them is still config and is scanned. The rest of the unit is still read and scanned, and so are unknown files everywhere else. A unit whose own directory is skipped (for example `~/.cache`) is still listed, as cache or state, under CACHE/STATE with `--all`. A `--root` that is or lies inside a skipped directory is walked as its own root.
- When any directory was skipped, the IGNORED line of the human report is followed by `N cache/state directories were not entered`. Each skipped directory counts once and nothing inside it is visited, so nested ones are not counted. The JSON has the count in `pruned_dirs` (always present, 0 when none). The `cache/state items` count on the IGNORED line (`cache_state_items` in the JSON) counts only the files that were listed in cache and state units, so it never includes files inside skipped directories. Skipped directories are not counted as depth-capped directories.
- By default units are scanned on one worker thread per CPU, at most 8. The hidden `--jobs <N>` flag (N at least 1) asks for N workers, for example `--jobs 1`; the worker count is capped at 64 and at the number of units, with at least one worker. The report is the same for any value. The walk and the committed-secrets scan each run on one thread.
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

Place a `.dredgeignore` file at the root of the dotfiles repository to list paths that are intentionally untracked. With several `--dotfiles` directories, the patterns of every `.dredgeignore` found at the root of a listed directory apply together (a directory without one adds nothing). Ignored paths are excluded from the untracked list only (and so from `--fail-on untracked`); drift and secrets are always reported. Each ignored item is counted in the summary line.

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

Pointing `--dotfiles` at a repository root that holds several stow directories makes each stow directory look like one package, so every path is wrong. When a stow-layout `--dotfiles` directory has a top level directory (one whose name does not start with a dot) that holds no dot entry of its own but has at least two subdirectories, among its first 32 subdirectories by name, that each hold a dot entry, dredge prints one `dredge: note:` line on stderr for it, saying it looks like a stow directory and suggesting `--dotfiles` with its path. Version control, Finder and tooling metadata (`.git`, `.gitignore`, `.gitattributes`, `.gitmodules`, `.gitkeep`, `.github` and `.DS_Store`) does not count as a dot entry in either place, so a folder of subprojects that each have a `.gitignore` or a `.git` gets no note. A directory that is already one of the `--dotfiles` is never suggested. The note never changes the report, the JSON or the exit code. Symlinks are not followed for this check, and a directory with more than 1,024 entries is not examined. Known false positive: a real package whose subfolders hold ordinary dot files, for example a `projects` package with `Development/.editorconfig` and `Work/.envrc`, also gets the note; ignore it in that case.

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

Directories named like caches (see "Limits") are not entered. A config rule for something inside such a directory must cover the whole directory (`dir/**`), or the walk never reaches the files it names.

Globs are relative to the home directory, use `/` separators, and `*` does not cross `/` (use `dir/**` for recursion). The first matching rule wins, so put narrower rules before broader ones. Only add paths you are sure about: an unclassified path is reported as unknown, which is better than a wrong classification. `cargo test` checks that every rule is complete, that no glob is duplicated, that no glob is shadowed by an earlier rule, and that no config rule names something inside a directory that is not entered.

Secret rules live in `rules/secrets.toml`. A new enabled rule needs a precise pattern and a detection case (a fake sample that fires only that rule, and a near miss that does not) in `tests/rules_schema.rs`. Build fake values from fragments so the repository holds no complete token-shaped string.

## License

MIT OR Apache-2.0. See `LICENSE-MIT`, `LICENSE-APACHE` and `NOTICE-THIRD-PARTY.md`.
