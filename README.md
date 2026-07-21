# WTFIS

**Where the fuck is** your project?

`wtfis` is a local-first, inline terminal project finder. `cdd` is its short alias. Type a project name, fix a typo, press Enter, and the shell changes into the selected directory.

## Install

```bash
brew tap prophesourvolodymyr/wtfis
brew install wtfis
```

Then add the shell integration once:

```bash
cat "$(brew --prefix)/share/wtfis/wtfis.zsh" >> ~/.zshrc
```

Restart your shell. For Bash, use `wtfis.bash` instead.

## Use

```bash
wtfis                    # open inline search
wtfis Mascotify          # search immediately
cdd Mascotify            # short alias
wtfis --set              # configure search roots
```

V1 uses local fuzzy matching and scans configured roots when it opens. It does not upload paths or project data. Semantic search is planned for V2.

## Development

```bash
cargo test
cargo run -- Mascotify
```

## License

MIT
