# dredge

**Find what your dotfiles don't know about.**

dredge scans your Mac's home directory, compares it with your dotfiles repo, and tells you three things:

- **What is not tracked.** Config that lives on your machine but not in your repo.
- **What has drifted.** Tracked files whose live copy no longer matches the repo.
- **Where the secrets are.** Private keys and API tokens, either sitting in untracked files or already committed to your repo.

It works with [stow](https://www.gnu.org/software/stow/) layouts, [chezmoi](https://www.chezmoi.io/) source directories, or no dotfiles manager at all.

It only reads. It never writes, moves or deletes anything, it has no network access, and it never prints a secret value.

## A quick look

```
$ dredge scan --dotfiles ~/dotfiles

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

That output comes from a made-up fixture home, not a real one. Each finding names the rule, the file and the line, and nothing else.

## Install

There is no release yet, so build from source with a stable Rust toolchain (1.88 or newer):

```sh
git clone https://github.com/dterana/dredge
cd dredge
cargo build --release
./target/release/dredge scan --help
```

The name `dredge` is a placeholder.

## Use it

```sh
# one stow or chezmoi directory
dredge scan --dotfiles ~/dotfiles

# several stow directories, for example a shared one plus one per machine
dredge scan --dotfiles ~/dotfiles-common --dotfiles ~/dotfiles-work

# machine readable, for scripts
dredge scan --json

# fail a script or a pre-push hook on secrets or drift (the default)
dredge scan --fail-on secrets,drift
```

The flags you will reach for most:

| Flag | What it does |
| --- | --- |
| `--dotfiles <PATH>` | A dotfiles directory. Repeat it for several. |
| `--layout <auto\|stow\|chezmoi>` | Force a layout. The default `auto` decides per directory. |
| `--watch <PATH>` | Force a secret scan of a path the normal walk skips, such as a folder inside an app's cache. |
| `--all` | Show everything, including tracked items. |
| `--json` | Emit JSON instead of the report. |
| `--fail-on <LIST>` | Which conditions give a non-zero exit: `secrets`, `drift`, `untracked`. |

The full flag list is in the [reference](docs/reference.md#flags).

### Silencing what you chose not to track

Put a `.dredgeignore` file at the root of your dotfiles repo, one glob per line, relative to your home directory:

```
.config/nvim/**
.zsh_sessions
```

Ignored paths leave the untracked list, but secrets and drift are always reported. The [full dialect](docs/reference.md#dredgeignore) is small.

### Exit codes

| Code | Meaning |
| --- | --- |
| 0 | None of the `--fail-on` conditions was hit. |
| 1 | At least one selected condition was hit. |
| 2 | dredge itself failed, for example a missing dotfiles directory. |

Untracked config alone exits 0 unless you pass `--fail-on untracked`.

## Why you can run it on a machine full of secrets

- **Read only.** It never writes, moves or deletes anything.
- **No secret values.** A finding holds a rule name, a path and a line number. There is no field for the value, so it cannot reach the terminal, the JSON or a log.
- **Symlinks are never followed**, and files are opened without blocking, so a link or a pipe cannot send it somewhere else or hang it.
- **Every read is bounded.** Files over 1 MiB and binary files are not scanned, and the walk depth is capped.
- **Linear-time patterns.** The secret rules use the `regex` crate's engine, with no backtracking.

## What it does not do

- It does not adopt files for you. It reports, and you run `stow` or `chezmoi add` yourself.
- It is macOS only for now.
- It scans the current working tree of your dotfiles repo, not past commits.
- It does not guess secrets by entropy. The generic rule ships disabled.

Cache and state folders (Electron storage, VS Code history and similar) are not entered, so a token stored there is not reported. Use `--watch` to scan one on purpose. The [reference](docs/reference.md#limits) lists every limit.

## Going further

- [Reference](docs/reference.md): every flag, every limit, the ignore dialect, and how stow and chezmoi layouts are read.
- **Contributing an app rule.** Rules live in `rules/apps.toml`. Adding one is a few lines of TOML:

  ```toml
  [[rule]]
  app = "myapp"
  kind = "config"            # config, state or cache
  globs = [".config/myapp/**"]
  description = "myapp configuration"
  ```

  Globs are relative to your home directory and use `/` separators. `*` does not cross `/`, so use `dir/**` to recurse. The first matching rule wins, so put narrower rules first. Only add paths you are sure about: an unclassified path is reported as unknown, which is better than a wrong classification. `cargo test` checks that every rule is complete, that no glob is duplicated or shadowed, and that no config rule names something inside a directory that is never entered.

  Secret rules live in `rules/secrets.toml`. A new enabled rule needs a precise pattern and a detection case (a fake sample that fires only that rule, and a near miss that does not) in `tests/rules_schema.rs`. Build fake values from fragments so the repository holds no complete token-shaped string.

## License

MIT OR Apache-2.0. See `LICENSE-MIT`, `LICENSE-APACHE` and `NOTICE-THIRD-PARTY.md`.
