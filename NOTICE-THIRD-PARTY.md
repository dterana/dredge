# Third-party notices

## Gitleaks

Seven secret detection patterns in `rules/secrets.toml` (those with `source = "gitleaks"`) are converted from the default configuration of gitleaks, https://github.com/gitleaks/gitleaks (`config/gitleaks.toml`). The conversion changed syntax only (Go regular expression escapes such as `\w` and `\d` replaced by explicit ASCII classes, and the surrounding capture group and non-capturing terminator removed). The patterns are:

- `github-fine-grained-pat` (gitleaks id `github-fine-grained-pat`)
- `gitlab-pat` (gitleaks id `gitlab-pat`)
- `slack-bot-token` (gitleaks id `slack-bot-token`)
- `slack-user-token` (gitleaks id `slack-user-token`)
- `slack-app-token` (gitleaks id `slack-app-token`)
- `pypi-upload-token` (gitleaks id `pypi-upload-token`)
- `shopify-access-token` (gitleaks id `shopify-access-token`)

Every other rule in `rules/secrets.toml` has `source = "dredge"`: it was written for this project from the publicly documented shape of the token, and makes no claim of derivation from gitleaks.

The gitleaks license text follows.

```
MIT License

Copyright (c) 2019 Zachary Rice

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## Rust crates

No source code from other projects is vendored in this repository. The compiled binary links the Rust crates listed in `Cargo.lock`; each crate is distributed under its own license, see its package metadata. This file does not reproduce those licenses.
