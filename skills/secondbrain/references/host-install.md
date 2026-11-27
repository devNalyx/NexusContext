# Installing the secondbrain adapter

The adapter is a portable Agent Skill. It follows the user, not a vault, so
install it in the agent's user-level skills directory rather than inside the
vault (where it would become indexed note content).

## Vendored official Obsidian skills

Install Obsidian's official skills with the route appropriate to the host:

- **Claude Code**:
  ```sh
  claude plugin marketplace add obsidianmd/obsidian-skills
  claude plugin install obsidian@obsidian-skills
  ```
  Inside a Claude Code session, use `/plugin marketplace add
  obsidianmd/obsidian-skills` then `/plugin install obsidian@obsidian-skills`.
  Update with `claude plugin update obsidian@obsidian-skills`.
- **Codex / ChatGPT**: add the Obsidian plugin from the ChatGPT plugin
  directory, or run `codex plugin marketplace add obsidianmd/obsidian-skills`
  and install Obsidian from the in-app plugin directory.
- **Cursor**: install the Obsidian plugin from the Cursor Marketplace.
- **OpenCode, GitHub Copilot and other Agent Skills hosts**:
  ```sh
  npx skills add https://github.com/obsidianmd/obsidian-skills
  ```
  The command prompts for target agents. Manual alternative: copy folders from
  `plugins/obsidian/skills/` into the agent's skills folder, e.g.
  `~/.copilot/skills/` for GitHub Copilot.
- Other documented routes include `agy plugin install
  https://github.com/obsidianmd/obsidian-skills` (Antigravity), `hermes plugins
  install obsidian` (Hermes) and `kiro-cli powers install obsidian` (Kiro).

Official skills default to the host's personal/global skills directory. Keep
them separate from this adapter so an Obsidian update cannot overwrite it.

## This adapter

From a NexusContext checkout:

```sh
npx skills add /path/to/NexusContext --skill secondbrain --agent opencode -g
```

Or install manually for OpenCode:

```sh
mkdir -p ~/.config/opencode/skills/secondbrain
cp -R skills/secondbrain/. ~/.config/opencode/skills/secondbrain/
```

Equivalent destinations for other hosts: `~/.claude/skills/secondbrain/`,
`~/.codex/skills/secondbrain/`, `~/.copilot/skills/secondbrain/`, or the host's
documented user-level skills folder. Do not place it inside the vault.

Verify with the host's skill listing command (for OpenCode, `/skill` in the TUI)
that `secondbrain` appears and that no vault note is mistaken for a skill.

Do not add `allowed-tools` to `SKILL.md`: the field is experimental in the
Agent Skills specification and the official Obsidian skills use only `name`
and `description`.
